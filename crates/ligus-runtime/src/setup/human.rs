//! The flows with a step only a person can do: add a box (a passkey
//! approval), add a phone (pairing in the person's own window), invite a
//! person (the invite code is shown only where it was made). The engine
//! never completes those steps; it checks before and after them.

use ligus_core::WorkspaceRole;

use super::agent::ops_code;
use super::{
    exit, gate, Ctx, Field, FieldKind, Inputs, Progress, Secrets, ServeFact, SetupOps, StepDef,
    Surface,
};

const ADMIT_WHERE: &str = "the desktop: workspace menu > Admit a box (or the phone)";

// ── Box ─────────────────────────────────────────────────────────────────────

pub(crate) fn box_status(ops: &dyn SetupOps) -> (Progress, String) {
    match ops.surface() {
        Surface::Desktop => {
            let n = ops.hosts().iter().filter(|h| h.kind == "worker").count();
            match n {
                0 => (Progress::Todo, "no boxes yet".into()),
                1 => (Progress::Done, "1 box".into()),
                n => (Progress::Done, format!("{n} boxes")),
            }
        }
        _ => match ops.box_state() {
            Ok(b) if b.joined => (Progress::Done, "this box is a member".into()),
            Ok(b) if b.desktop_data_dir => (
                Progress::Unavailable,
                "this is the desktop app's data dir".into(),
            ),
            Ok(_) => (Progress::Todo, "this box isn't admitted yet".into()),
            Err(_) => (Progress::Unavailable, "run it on the box".into()),
        },
    }
}

pub(crate) fn box_steps(ops: &dyn SetupOps) -> Vec<StepDef> {
    match ops.surface() {
        Surface::Desktop => vec![
            StepDef::person("admit", "Admit the box", "Admit a box: paste the box's join request, check its fingerprint, approve with a passkey"),
            StepDef::verify("verify", "Box joined"),
        ],
        _ => vec![
            StepDef::input(
                "request",
                "Join request",
                vec![Field::new("label", "Label", FieldKind::Text).help("How the workspace names this box (its host name by default).")],
            ),
            StepDef::person("approve", "An Admin approves", ADMIT_WHERE),
            StepDef::input(
                "redeem",
                "Admission",
                vec![Field::new("code", "Admission code", FieldKind::Secret)
                    .help("The sealed code the Admin sends back. It carries the workspace key: pass it on stdin or in an owner-only file.")],
            ),
            StepDef::verify("verify", "Serving"),
        ],
    }
}

pub(crate) fn box_preflight(ctx: &mut Ctx, ops: &dyn SetupOps) {
    match ops.surface() {
        Surface::Desktop => {
            if !gate::need(ctx, ops, WorkspaceRole::Admin, "admitting a box") {
                return;
            }
            if ops.my_role().is_none() {
                return ctx.fail(
                    "workspace",
                    "workspace",
                    "no workspace yet",
                    "create or join a workspace first",
                    exit::FIXABLE,
                );
            }
            match ops.relay_url() {
                Some(r) => ctx.ok("relay", "relay", super::validate::origin(&r)),
                None => ctx.fail(
                    "relay",
                    "relay",
                    "this workspace has no relay, so a box can't reach it",
                    "set up a relay first (Set up a relay)",
                    exit::FIXABLE,
                ),
            }
        }
        _ => {
            let b = match ops.box_state() {
                Ok(b) => b,
                Err(e) => {
                    return ctx.fail(
                        "box",
                        "box",
                        e.to_string(),
                        "run `ligus setup box` on the box itself",
                        exit::FIXABLE,
                    )
                }
            };
            if b.desktop_data_dir {
                return ctx.fail(
                    "data-dir",
                    "data dir",
                    "this is the desktop app's data dir: one data dir is one device",
                    "give the box its own: LIGUS_DATA_DIR=~/.ligus-serve ligus setup box",
                    exit::FIXABLE,
                );
            }
            ctx.ok("data-dir", "data dir", "a separate data dir");
            let sb = ops.sandbox();
            if !sb.confined() {
                return ctx.fail(
                    "sandbox",
                    "agent sandbox",
                    "none on this box: ligus serve refuses to run agents without one",
                    "ligus doctor --sandbox says what is missing (Linux: bwrap, or Landlock on 6.7+)",
                    exit::FIXABLE,
                );
            }
            ctx.ok("sandbox", "agent sandbox", sb.tier);
        }
    }
}

pub(crate) fn box_run(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs, secrets: &Secrets) {
    if ops.surface() == Surface::Desktop {
        let n = ops.hosts().iter().filter(|h| h.kind == "worker").count();
        if n > 0 {
            ctx.ok("hosts", "boxes", format!("{n} already in the workspace"));
        }
        return ctx.person(
            "admit",
            "admit",
            "on the box, run `ligus setup box`; it prints a join request",
            "open Admit a box, paste the request, check the fingerprint and approve with a passkey",
        );
    }
    let b = match ops.box_state() {
        Ok(b) => b,
        Err(e) => {
            return ctx.fail(
                "box",
                "box",
                e.to_string(),
                "run `ligus setup box` on the box itself",
                exit::FIXABLE,
            )
        }
    };
    if b.joined {
        ctx.ok(
            "joined",
            "member",
            format!(
                "this box is a member of {}",
                b.workspace.as_deref().unwrap_or("its workspace")
            ),
        );
        return verify_serving(ctx, ops);
    }
    if let Some(code) = secrets.get("code") {
        if ctx.cancelled() {
            return;
        }
        if ctx.dry_run() {
            ctx.skip(
                "redeem",
                "admission",
                "would keep the admission for this box",
            );
            return ctx.done();
        }
        match ops.redeem(code) {
            Ok(ws) => ctx.ok(
                "redeem",
                "admission",
                format!("kept for {ws}; the box joins when it starts"),
            ),
            Err(e) => {
                return ctx.fail(
                    "redeem",
                    "admission",
                    e.to_string(),
                    "ask the Admin for a new admission (they expire, and each is single use)",
                    ops_code(&e),
                )
            }
        }
        ctx.commit();
        return verify_serving(ctx, ops);
    }
    let label = inputs.get("label").unwrap_or("").to_string();
    if ctx.dry_run() {
        ctx.skip(
            "request",
            "join request",
            "would print this box's join request",
        );
        return ctx.done();
    }
    match ops.join_request(&label) {
        Ok((request, fingerprint)) => {
            ctx.ok(
                "request",
                "join request",
                format!("fingerprint {fingerprint}"),
            );
            // Public key material and a label: safe to show and to log.
            ctx.notes(vec![request.clone()]);
            ctx.result("joinRequest", request);
            ctx.result("fingerprint", fingerprint.clone());
            ctx.person(
                "approve",
                "approval",
                format!("an Admin approves this box with a passkey on {ADMIT_WHERE}, checking fingerprint {fingerprint}"),
                "when they send the sealed code back: ligus setup box --code - (or --code @FILE)",
            );
        }
        Err(e) => ctx.fail(
            "request",
            "join request",
            e.to_string(),
            "ligus admit request prints it too",
            ops_code(&e),
        ),
    }
}

fn verify_serving(ctx: &mut Ctx, ops: &dyn SetupOps) {
    match ops.serve_running() {
        ServeFact::Running(at) => {
            ctx.ok("serve", "ligus serve", format!("answering on {at}"));
            ctx.done();
        }
        ServeFact::NotRunning(why) => {
            ctx.warn(
                "serve",
                "ligus serve",
                format!("not answering: {why}"),
                Some(
                    "start it: deploy/local/ligus-local start --role serve (or ligus serve)".into(),
                ),
            );
            ctx.next("deploy/local/ligus-local start --role serve");
            ctx.done();
        }
    }
}

// ── Phone ───────────────────────────────────────────────────────────────────

pub(crate) fn phone_status(ops: &dyn SetupOps) -> (Progress, String) {
    match ops.phones().len() {
        0 => (Progress::Todo, "no phone paired".into()),
        1 => (Progress::Done, "1 phone".into()),
        n => (Progress::Done, format!("{n} phones")),
    }
}

pub(crate) fn phone_steps(ops: &dyn SetupOps) -> Vec<StepDef> {
    match ops.surface() {
        Surface::Desktop => vec![
            StepDef::person(
                "pair",
                "Pair",
                "Add a phone: scan the code in this window with the phone app",
            ),
            StepDef::verify("verify", "Paired"),
        ],
        _ => vec![
            StepDef::person("pair", "Pair", "your own terminal on the box: ligus pair"),
            StepDef::verify("verify", "Paired"),
        ],
    }
}

pub(crate) fn phone_preflight(ctx: &mut Ctx, ops: &dyn SetupOps) {
    match ops.phone_precheck() {
        Ok(checks) => {
            let blocked = checks
                .iter()
                .any(|c| c.status == crate::checks::Status::Fail);
            ctx.checks.extend(checks);
            if blocked {
                ctx.stop(exit::FIXABLE);
            }
        }
        Err(super::OpsError::Unsupported) => {}
        Err(e) => ctx.fail(
            "precheck",
            "precheck",
            e.to_string(),
            "ligus doctor checks this device",
            ops_code(&e),
        ),
    }
}

pub(crate) fn phone_run(ctx: &mut Ctx, ops: &mut dyn SetupOps) {
    let phones = ops.phones();
    if !phones.is_empty() {
        ctx.ok("paired", "phones", phones.join(", "));
    }
    if ctx.dry_run() {
        ctx.skip("pair", "pair", "would hand over to pairing");
        return ctx.done();
    }
    // The pairing secret is shown only in the person's own window: never by
    // this flow, never in its output.
    match ops.surface() {
        Surface::Desktop => ctx.person(
            "pair",
            "pair",
            "pairing shows a code only in this window",
            "open Add a phone and scan the code with the phone app",
        ),
        _ => ctx.person(
            "pair",
            "pair",
            "pairing prints a secret link: run it where only you see it, never in a log or a job",
            "in your own terminal on the box: ligus pair",
        ),
    }
}

// ── Invite ──────────────────────────────────────────────────────────────────

/// What an invited person may do, in one line (a workspace invite joins
/// them as a Contributor; an Admin changes the role in People).
pub const INVITE_ROLE: &str = "They join as a Contributor: they chat, run agents and propose changes. An Admin can change the role in People.";

pub(crate) fn invite_status(ops: &dyn SetupOps) -> (Progress, String) {
    let inv = ops.invites();
    let waiting = inv.iter().filter(|i| !i.used && !i.closed).count();
    let used = inv.iter().filter(|i| i.used).count();
    match (waiting, used) {
        (0, 0) => (Progress::Todo, "no invites yet".into()),
        (0, u) => (Progress::Done, format!("{u} accepted")),
        (w, u) => (Progress::Waiting, format!("{w} waiting, {u} accepted")),
    }
}

pub(crate) fn invite_steps(ops: &dyn SetupOps) -> Vec<StepDef> {
    let where_ = if ops.surface() == Surface::Desktop {
        "this window: the code is shown once, to copy"
    } else {
        "the desktop: People > Invite someone"
    };
    vec![
        StepDef::person("create", "Create the invite", where_),
        StepDef::verify("track", "Accepted"),
    ]
}

pub(crate) fn invite_preflight(ctx: &mut Ctx, ops: &dyn SetupOps) {
    match ops.relay_url() {
        Some(r) => ctx.ok("relay", "relay", super::validate::origin(&r)),
        None => ctx.fail(
            "relay",
            "relay",
            "this workspace has no relay, so nobody else could reach it",
            "set up a relay first (ligus setup relay)",
            exit::FIXABLE,
        ),
    }
}

pub(crate) fn invite_run(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs) {
    let _ = inputs;
    ctx.ok("role", "role", INVITE_ROLE);
    let inv = ops.invites();
    let waiting = inv.iter().filter(|i| !i.used && !i.closed).count();
    let used = inv.iter().filter(|i| i.used).count();
    if used > 0 || waiting > 0 {
        ctx.ok(
            "track",
            "invites",
            format!("{waiting} waiting, {used} accepted"),
        );
    }
    if ctx.dry_run() {
        ctx.skip("create", "invite", "would hand over to creating an invite");
        return ctx.done();
    }
    // Creating an invite is an Admin's approval (a passkey where the
    // workspace asks for one), and the code is a bearer secret: it is made
    // and shown only where a person creates it.
    match ops.surface() {
        Surface::Desktop => ctx.person("create", "invite", "the code is shown once, in this window, to copy", "create the invite here and send it to the person"),
        _ => ctx.person(
            "create",
            "invite",
            "the CLI doesn't create invites: the code carries the workspace key",
            "on the desktop: People > Invite someone; then `ligus setup invite` shows when it is accepted",
        ),
    }
}

//! Who may run a flow: the role gate and the driver gate. The underlying
//! writes are still authorized by the event fold; these give a clear answer
//! before anything happens.

use ligus_core::WorkspaceRole;

use super::{exit, role_name, Ctx, Driver, FlowId, Secrets, SetupOps};

/// `Err(why)` when this device's role can't run `flow` at all.
pub(crate) fn role_gate(flow: FlowId, ops: &dyn SetupOps) -> Result<(), String> {
    let Some(min) = flow.def().min_role else { return Ok(()) };
    match ops.my_role() {
        // Before a workspace exists only the local flows make sense; the
        // ones that need a role say so in their preflight.
        None if matches!(flow, FlowId::Agent | FlowId::Folder) => Ok(()),
        None => Err("join or create a workspace first".into()),
        Some(r) if r.rank() >= min.rank() => Ok(()),
        Some(r) => Err(format!("needs {} or above (you are {})", role_name(min), role_name(r))),
    }
}

pub(crate) fn run_gate(ctx: &mut Ctx, ops: &dyn SetupOps) {
    if let Err(why) = role_gate(ctx.flow, ops) {
        let code = if ops.my_role().is_none() { exit::FIXABLE } else { exit::REFUSED };
        let fix = if code == exit::REFUSED {
            "ask an Admin of the workspace to run it, or to change your role".to_string()
        } else {
            "create or join a workspace first (desktop: workspace menu; CLI: ligus enroll)".to_string()
        };
        ctx.fail("role", "role", why, fix, code);
    }
}

/// An agent may read forms and status but run no flow (design §6): letting
/// one run a flow needs a person's confirm step before the first side
/// effect, which is a later change.
pub(crate) fn driver_gate(ctx: &mut Ctx, secrets: &Secrets) {
    if ctx.driver() == Driver::Agent {
        let what = if secrets.is_empty() { "an agent can't run a setup flow" } else { "an agent can't pass secrets to a setup flow" };
        ctx.fail(
            "driver",
            "who runs it",
            what,
            format!("a person runs `ligus setup {}` or the desktop's {}", ctx.flow.as_str(), ctx.flow.def().title),
            exit::REFUSED,
        );
    }
}

/// Raise the gate inside a flow (a step that needs more than the flow).
pub(crate) fn need(ctx: &mut Ctx, ops: &dyn SetupOps, min: WorkspaceRole, what: &str) -> bool {
    match ops.my_role() {
        Some(r) if r.rank() >= min.rank() => true,
        None => true, // no workspace yet: the write itself decides
        Some(r) => {
            ctx.fail(
                "role",
                "role",
                format!("{what} needs {} or above (you are {})", role_name(min), role_name(r)),
                "ask an Admin of the workspace",
                exit::REFUSED,
            );
            false
        }
    }
}

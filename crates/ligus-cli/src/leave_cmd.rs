//! `ligus serve leave`: this box leaves its workspace on its own
//! (`MemberLeft`, workspace model v3, 10.4), signed by its own device key.
//! Under rules 3 the box stays listed as "asked to leave" until an Admin
//! removes it. Leaving isn't revoking: the box keeps what it holds until an
//! Admin removes it and rotates the workspace key.

use anyhow::Result;
use ligus_runtime::chat_service::{LeaveCheck, LEAVE_CONSEQUENCE, NOT_REVOKING};

use crate::checks::{Check, Report};
use crate::problem::{Problem, EXIT_HUMAN, EXIT_REFUSED};
use crate::{uuid_of_room, Config};

fn problem(exit: i32, code: &'static str, message: impl Into<String>, fix: Option<&str>) -> anyhow::Error {
    anyhow::Error::new(Problem { exit, code, message: message.into(), fix: fix.map(str::to_string) })
}

fn print(report: &Report, code: i32, json: bool, extra: serde_json::Value, text: &str) {
    if json {
        let mut v = report.to_json(code);
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.insert("result".into(), serde_json::Value::Object(e.clone()));
        }
        println!("{v}");
    } else {
        print!("{text}");
    }
}

/// The one line `ligus serve leave` shows before it acts.
pub fn consequence(workspace: &str) -> String {
    format!("This box leaves {workspace}. {LEAVE_CONSEQUENCE} {NOT_REVOKING}")
}

/// `ligus serve leave [--yes] [--json]`.
pub async fn cmd_serve_leave(cfg: &Config, yes: bool, json: bool) -> Result<()> {
    let _ = crate::sync_once(cfg).await;
    let mut svc = crate::open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let line = consequence(&cfg.room);
    match svc.leave_check(ws).map_err(|e| problem(crate::problem::EXIT_ERROR, "leave", e.to_string(), None))? {
        LeaveCheck::AlreadyLeft => {
            let mut r = Report::new("serve leave");
            r.add(Check::ok("leave.done", "left", "this box already asked to leave; an Admin removes it from People"));
            r.next("stop the box (deploy/local/ligus-local stop, or your service manager)");
            print(&r, 0, json, serde_json::json!({ "left": true, "changed": false }), "This box already asked to leave.\n");
            return Ok(());
        }
        LeaveCheck::Refused(why) => return Err(problem(EXIT_REFUSED, "refused", format!("couldn't leave: {why}"), Some("ask an Admin to remove this box from People instead"))),
        LeaveCheck::NeedsPasskey => {
            return Err(problem(
                EXIT_REFUSED,
                "passkey",
                "this member holds a passkey, so it leaves from its person's desktop (Leave workspace), not from a box",
                None,
            ))
        }
        LeaveCheck::Ready => {}
    }
    if !yes {
        return Err(problem(EXIT_HUMAN, "confirm", line, Some("run again with --yes to leave")));
    }
    svc.leave_workspace(ws).map_err(|e| problem(EXIT_REFUSED, "refused", format!("couldn't leave: {e}"), None))?;
    drop(svc);
    let pushed = crate::sync_once(cfg).await.is_ok();
    let mut r = Report::new("serve leave");
    r.add(Check::ok("leave.done", "left", LEAVE_CONSEQUENCE));
    if pushed {
        r.add(Check::ok("leave.synced", "synced", "the leave reached the relay"));
    } else {
        r.add(Check::warn("leave.synced", "not synced yet", "the leave is written here and goes out on the next sync"));
    }
    r.add(Check::warn("leave.keys", "keys kept", NOT_REVOKING));
    r.next("stop the box (deploy/local/ligus-local stop, or your service manager)");
    r.next("an Admin removes it from People and rotates the workspace key to cut what it holds");
    let text = format!("Left {}. {LEAVE_CONSEQUENCE}\n{NOT_REVOKING}\nNext: stop the box, then ask an Admin to remove it from People.\n", cfg.room);
    print(&r, 0, json, serde_json::json!({ "left": true, "changed": true, "synced": pushed }), &text);
    Ok(())
}

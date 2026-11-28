//! `ligus serve leave`: this box leaves its workspace on its own
//! (`MemberLeft`, workspace model v3, 10.4), signed by its own device key.
//! Under rules 3 the box stays listed as "asked to leave" until an Admin
//! removes it; under rules 4 the leave removes it. Leaving isn't revoking:
//! the box keeps what it holds until an Admin rotates the workspace key.

use anyhow::Result;
use ligus_runtime::chat_service::{leave_consequence, LeaveCheck, NOT_REVOKING};

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

/// Whether `rules` makes a leave a removal (rules 4).
fn removes(rules: u32) -> bool {
    rules >= ligus_core::authz::RULES_V4
}

/// The one line `ligus serve leave` shows before it acts, on `rules`.
pub fn consequence(rules: u32, workspace: &str) -> String {
    format!("This box leaves {workspace}. {} {NOT_REVOKING}", leave_consequence(rules, workspace))
}

/// `ligus serve leave [--yes] [--json]`.
pub async fn cmd_serve_leave(cfg: &Config, yes: bool, json: bool) -> Result<()> {
    let _ = crate::sync_once(cfg).await;
    let mut svc = crate::open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let rules = svc.leave_rules(ws).map_err(|e| problem(crate::problem::EXIT_ERROR, "leave", e.to_string(), None))?;
    let line = consequence(rules, &cfg.room);
    match svc.leave_check(ws).map_err(|e| problem(crate::problem::EXIT_ERROR, "leave", e.to_string(), None))? {
        LeaveCheck::AlreadyLeft if removes(rules) => {
            let mut r = Report::new("serve leave");
            r.add(Check::ok("leave.done", "left", format!("this box already left and was removed from {}", cfg.room)));
            r.next("stop the box (deploy/local/ligus-local stop, or your service manager)");
            r.next("to rejoin: ligus admit request (an Admin approves)");
            print(&r, 0, json, serde_json::json!({ "left": true, "changed": false }), &format!("{}\n", crate::admit::left_line(&cfg.room)));
            return Ok(());
        }
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
    let said = leave_consequence(rules, &cfg.room);
    let mut r = Report::new("serve leave");
    r.add(Check::ok("leave.done", "left", said.clone()));
    if pushed {
        r.add(Check::ok("leave.synced", "synced", "the leave reached the relay"));
    } else {
        r.add(Check::warn("leave.synced", "not synced yet", "the leave is written here and goes out on the next sync"));
    }
    r.add(Check::warn("leave.keys", "keys kept", NOT_REVOKING));
    r.next("stop the box (deploy/local/ligus-local stop, or your service manager)");
    let next = if removes(rules) {
        r.next("an Admin rotates the workspace key to cut what it holds");
        r.next("to rejoin: ligus admit request (an Admin approves)");
        "Next: stop the box. To rejoin: ligus admit request (an Admin approves)."
    } else {
        r.next("an Admin removes it from People and rotates the workspace key to cut what it holds");
        "Next: stop the box, then ask an Admin to remove it from People."
    };
    let text = format!("Left {}. {said}\n{NOT_REVOKING}\n{next}\n", cfg.room);
    print(&r, 0, json, serde_json::json!({ "left": true, "changed": true, "synced": pushed }), &text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_says_rules_4_removes_the_box_and_rules_3_keeps_its_wording() {
        let r4 = consequence(ligus_core::authz::RULES_V4, "home");
        assert!(r4.starts_with("This box leaves home. You'll be removed from home, and this device stops syncing it."), "{r4}");
        assert!(!r4.contains("until an Admin removes"), "{r4}");
        let r3 = consequence(ligus_core::authz::RULES_V3, "home");
        assert!(r3.contains("asked to leave until an Admin removes you"), "{r3}");
    }
}

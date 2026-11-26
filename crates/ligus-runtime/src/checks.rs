//! One check report shape for every Ligus tool: `ligus.checks.v1`
//! (docs-site/docs/operate/reference/cli-conventions.md).
//!
//! `ligus doctor --json` prints one [`Report`]; the text form is the same
//! report for a person: one line per check (`ok`, `warn`, `fail` or `skip`,
//! the title, what was found), an indented `fix:` line, then a summary.
//! The deploy scripts' `ligus.agent-deploy.v1` is a conforming profile of the
//! same shape (deploy/lib/agent-common.sh), and `langstroth-relay check
//! --json` prints it too.

use std::fmt::Write as _;

use serde_json::{json, Value};

/// The schema string every report carries.
pub const SCHEMA: &str = "ligus.checks.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Skip => "skip",
        }
    }
}

/// One check: a stable dotted id, a short title for the text view, what was
/// found, and how to fix it when it isn't `ok`.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub detail: String,
    pub fix: Option<String>,
    /// More lines of what was found (sandbox notes, the turn test's
    /// findings), in order.
    pub notes: Vec<String>,
}

impl Check {
    pub fn new(id: &str, title: &str, status: Status, detail: impl Into<String>) -> Self {
        Check { id: id.into(), title: title.into(), status, detail: one_line(&detail.into()), fix: None, notes: Vec::new() }
    }
    pub fn ok(id: &str, title: &str, detail: impl Into<String>) -> Self {
        Self::new(id, title, Status::Ok, detail)
    }
    pub fn warn(id: &str, title: &str, detail: impl Into<String>) -> Self {
        Self::new(id, title, Status::Warn, detail)
    }
    pub fn fail(id: &str, title: &str, detail: impl Into<String>) -> Self {
        Self::new(id, title, Status::Fail, detail)
    }
    pub fn skip(id: &str, title: &str, detail: impl Into<String>) -> Self {
        Self::new(id, title, Status::Skip, detail)
    }
    pub fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(one_line(&fix.into()));
        self
    }
    pub fn notes(mut self, notes: impl IntoIterator<Item = String>) -> Self {
        self.notes.extend(notes);
        self
    }

    pub fn to_json(&self) -> Value {
        let mut o = json!({ "id": self.id, "title": self.title, "status": self.status.as_str(), "detail": self.detail });
        if let Some(f) = &self.fix {
            o["fix"] = json!(f);
        }
        // Blank lines only space out the text view.
        let notes: Vec<&String> = self.notes.iter().filter(|n| !n.trim().is_empty()).collect();
        if !notes.is_empty() {
            o["notes"] = json!(notes);
        }
        o
    }
}

/// Details and fixes are one line each (the text view and `jq -r` rely on it).
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A whole run: the checks in order and the commands to run next.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub command: String,
    pub checks: Vec<Check>,
    pub next: Vec<String>,
}

impl Report {
    pub fn new(command: &str) -> Self {
        Report { command: command.into(), ..Default::default() }
    }
    pub fn add(&mut self, c: Check) {
        self.checks.push(c);
    }
    /// A next step, once (in the order first added).
    pub fn next(&mut self, cmd: impl Into<String>) {
        let cmd = cmd.into();
        if !self.next.contains(&cmd) {
            self.next.push(cmd);
        }
    }
    /// No check failed (warnings allowed).
    pub fn ok(&self) -> bool {
        !self.checks.iter().any(|c| c.status == Status::Fail)
    }
    pub fn count(&self, s: Status) -> usize {
        self.checks.iter().filter(|c| c.status == s).count()
    }

    /// The `ligus.checks.v1` document. `exit_code` is the code the process
    /// exits with.
    pub fn to_json(&self, exit_code: i32) -> Value {
        json!({
            "schema": SCHEMA,
            "tool": "ligus",
            "version": env!("CARGO_PKG_VERSION"),
            "command": self.command,
            "ok": self.ok(),
            "exitCode": exit_code,
            "checks": self.checks.iter().map(Check::to_json).collect::<Vec<_>>(),
            "next": self.next,
        })
    }

    /// One check in the text form.
    pub fn check_text(c: &Check) -> String {
        Self::check_text_styled(c, false)
    }

    /// One check in the text form; with `color`, a failure's status in red
    /// and a warning's in yellow (the word stays, for no colour).
    pub fn check_text_styled(c: &Check, color: bool) -> String {
        let word = format!("{:<5}", c.status.as_str());
        let word = match (color, c.status) {
            (true, Status::Fail) => format!("\x1b[1;31m{word}\x1b[0m"),
            (true, Status::Warn) => format!("\x1b[33m{word}\x1b[0m"),
            _ => word,
        };
        let mut s = format!("{word} {:<15} {}\n", c.title, c.detail);
        for n in &c.notes {
            let _ = writeln!(s, "      {n}");
        }
        if let (Some(f), false) = (&c.fix, c.status == Status::Ok) {
            let _ = writeln!(s, "      fix: {f}");
        }
        s
    }

    /// The summary line: `2 warnings · next: nothing to do`.
    pub fn summary(&self) -> String {
        let (f, w) = (self.count(Status::Fail), self.count(Status::Warn));
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let head = match (f, w) {
            (0, 0) => "all checks passed".to_string(),
            (0, w) => plural(w, "warning", "warnings"),
            (f, 0) => plural(f, "failure", "failures"),
            (f, w) => format!("{}, {}", plural(f, "failure", "failures"), plural(w, "warning", "warnings")),
        };
        let next = if self.next.is_empty() { "nothing to do".to_string() } else { self.next.join(" ; ") };
        format!("{head} · next: {next}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Report {
        let mut r = Report::new("doctor");
        r.add(Check::ok("sandbox.tier", "agent sandbox", "landlock"));
        r.add(Check::warn("workspace.joined", "workspace", "not joined").fix("ligus enroll --file ./invite"));
        r.add(Check::fail("relay.reachable", "relay", "connection\nrefused").fix("langstroth-relay check --url https://r.example"));
        r.next("langstroth-relay check --url https://r.example");
        r.next("langstroth-relay check --url https://r.example");
        r
    }

    #[test]
    fn the_document_has_every_field_of_the_schema() {
        let d = sample().to_json(1);
        assert_eq!(d["schema"], SCHEMA);
        assert_eq!(d["tool"], "ligus");
        assert_eq!(d["command"], "doctor");
        assert_eq!(d["ok"], false);
        assert_eq!(d["exitCode"], 1);
        assert!(d["version"].is_string());
        assert_eq!(d["next"].as_array().unwrap().len(), 1, "a next step is listed once");
        for c in d["checks"].as_array().unwrap() {
            let keys: Vec<&str> = c.as_object().unwrap().keys().map(String::as_str).collect();
            assert!(keys.iter().all(|k| ["id", "title", "status", "detail", "fix", "notes"].contains(k)), "{c}");
            assert!(["ok", "warn", "fail", "skip"].contains(&c["status"].as_str().unwrap()));
            if c["status"] == "fail" {
                assert!(c["fix"].is_string(), "a failed check says what to do: {c}");
            }
            assert!(!c["detail"].as_str().unwrap().contains('\n'));
        }
    }

    #[test]
    fn warnings_alone_are_ok() {
        let mut r = Report::new("doctor");
        r.add(Check::warn("a", "a", "x"));
        r.add(Check::skip("b", "b", "y"));
        assert!(r.ok());
        assert_eq!(r.summary(), "1 warning · next: nothing to do");
        assert_eq!(sample().summary(), "1 failure, 1 warning · next: langstroth-relay check --url https://r.example");
    }

    #[test]
    fn the_text_form_puts_the_fix_under_the_check() {
        let r = sample();
        let t = Report::check_text(&r.checks[2]);
        assert_eq!(t, "fail  relay           connection refused\n      fix: langstroth-relay check --url https://r.example\n");
        assert!(!Report::check_text(&Check::ok("x", "x", "fine").fix("never shown")).contains("fix:"));
        let red = Report::check_text_styled(&r.checks[2], true);
        assert!(red.starts_with("\x1b[1;31mfail \x1b[0m relay"), "{red:?}");
        assert!(!Report::check_text_styled(&r.checks[0], true).contains('\x1b'), "ok is never coloured");
    }
}

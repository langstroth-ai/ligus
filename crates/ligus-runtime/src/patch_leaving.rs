//! The patch-leaving rule for long agent loops.
//!
//! A forager leaves a flower patch when its gain rate drops below what the
//! rest of the environment offers (Charnov's marginal value theorem), and a
//! honeybee, whose lifetime flight budget is what binds, weighs efficiency
//! rather than raw rate. An agent burning a plan window in a long tool loop
//! should do the same: when its tool calls stop producing measurable progress
//! (tests newly passing, files actually changing), it stops and checks in with
//! the person instead of spending the window on more of the same.
//!
//! [`ProgressTracker`] watches the tool calls Ligus itself runs (the built-in
//! file tools and MCP, for API and local models). Progress is measured, never
//! claimed by the agent:
//!   * a write or edit that changes a file's content (rewriting the same bytes
//!     is not progress);
//!   * a test run whose passing count beats the best seen this turn.
//!
//! After `stall_limit` consecutive calls with no progress (once at least that
//! many calls have run), every further call gets a check-in instruction
//! instead of running, so the model stops and reports.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde_json::Value;

/// Default number of tool calls without progress before a check-in.
pub const DEFAULT_STALL_LIMIT: usize = 12;

/// What an agent gets once it has stopped making progress.
pub const CHECK_IN: &str = "Check-in: the last tool calls in this turn produced no measurable progress \
(no file changed, no more tests passing). Stop calling tools now. Reply with what you tried, what is \
blocking you, and what you need from the person to continue.";

/// The stall limit from `LIGUS_PATCH_LEAVING_CALLS` (0 disables the rule).
pub fn stall_limit_from_env() -> usize {
    ligus_core::names::var("LIGUS_PATCH_LEAVING_CALLS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_STALL_LIMIT)
}

#[derive(Debug, Default)]
struct State {
    calls: usize,
    since_progress: usize,
    units: f64,
    best_passing: u64,
    file_states: HashSet<(String, u64)>,
    checked_in: bool,
    tests_units: f64,
    write_units: f64,
}

/// Measured progress of one turn's tool loop.
#[derive(Debug)]
pub struct ProgressTracker {
    stall_limit: usize,
    state: Mutex<State>,
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::new(stall_limit_from_env())
    }
}

fn passing_re() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            // cargo: "test result: ok. 12 passed; 0 failed"
            Regex::new(r"(\d+) passed;").unwrap(),
            // pytest / vitest / jest: "12 passed"
            Regex::new(r"\b(\d+) passed\b").unwrap(),
            // go test -v: count "--- PASS"
            Regex::new(r"(?m)^\s*--- PASS").unwrap(),
        ]
    })
}

/// Passing-test count in a command's output, if it looks like a test run.
pub fn passing_tests(output: &str) -> Option<u64> {
    let res = passing_re();
    for re in &res[..2] {
        let total: u64 = re.captures_iter(output).filter_map(|c| c[1].parse::<u64>().ok()).sum();
        if total > 0 {
            return Some(total);
        }
    }
    let go = res[2].find_iter(output).count() as u64;
    (go > 0).then_some(go)
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

impl ProgressTracker {
    pub fn new(stall_limit: usize) -> Self {
        Self { stall_limit, state: Mutex::new(State::default()) }
    }

    /// Before running a call: `Some(check-in text)` when the loop should stop.
    pub fn gate(&self) -> Option<&'static str> {
        if self.stall_limit == 0 {
            return None;
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.checked_in || (st.calls >= self.stall_limit && st.since_progress >= self.stall_limit) {
            st.checked_in = true;
            return Some(CHECK_IN);
        }
        None
    }

    /// After a call ran: measure whether it made progress.
    pub fn observe(&self, name: &str, input: &Value, output: &str, is_error: bool) {
        use crate::builtin_tools::{EDIT_FILE, RUN_COMMAND, WRITE_FILE};
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.calls += 1;
        let mut progressed = false;
        if !is_error && (name == WRITE_FILE || name == EDIT_FILE) {
            // A write counts when it puts content we haven't seen at that path.
            let path = input.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let content = input
                .get("content")
                .or_else(|| input.get("new_string"))
                .or_else(|| input.get("new_text"))
                .map(Value::to_string)
                .unwrap_or_default();
            if st.file_states.insert((path, fnv(&content))) {
                st.units += 1.0;
                st.write_units += 1.0;
                progressed = true;
            }
        }
        if name == RUN_COMMAND {
            if let Some(n) = passing_tests(output) {
                if n > st.best_passing {
                    let gain = (n - st.best_passing) as f64;
                    st.best_passing = n;
                    st.units += gain;
                    st.tests_units += gain;
                    progressed = true;
                }
            }
        }
        st.since_progress = if progressed { 0 } else { st.since_progress + 1 };
    }

    /// Progress units so far, split as (tests newly passing, files written).
    pub fn units(&self) -> (f64, f64) {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (st.tests_units, st.write_units)
    }

    /// Whether the rule made the agent check in this turn.
    pub fn checked_in(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).checked_in
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_tools::{READ_FILE, RUN_COMMAND, WRITE_FILE};
    use serde_json::json;

    #[test]
    fn counts_passing_tests_across_formats() {
        assert_eq!(passing_tests("test result: ok. 12 passed; 0 failed;\ntest result: ok. 3 passed; 0 failed;"), Some(15));
        assert_eq!(passing_tests("==== 7 passed, 1 warning in 0.3s ===="), Some(7));
        assert_eq!(passing_tests("--- PASS: TestA\n--- PASS: TestB\n"), Some(2));
        assert_eq!(passing_tests("compiling..."), None);
    }

    #[test]
    fn a_stalled_loop_checks_in() {
        let t = ProgressTracker::new(3);
        for _ in 0..3 {
            assert!(t.gate().is_none());
            t.observe(READ_FILE, &json!({"path": "a"}), "x", false);
        }
        assert_eq!(t.gate(), Some(CHECK_IN));
        // Once checked in, it stays checked in for the turn.
        assert_eq!(t.gate(), Some(CHECK_IN));
        assert!(t.checked_in());
    }

    #[test]
    fn real_progress_resets_the_clock_and_repeats_do_not() {
        let t = ProgressTracker::new(3);
        let w = json!({"path": "src/a.rs", "content": "v1"});
        t.observe(WRITE_FILE, &w, "ok", false);
        t.observe(READ_FILE, &json!({}), "", false);
        t.observe(READ_FILE, &json!({}), "", false);
        assert!(t.gate().is_none(), "the write was progress");
        // Rewriting the same bytes is not progress.
        t.observe(WRITE_FILE, &w, "ok", false);
        assert_eq!(t.gate(), Some(CHECK_IN));

        let t = ProgressTracker::new(2);
        t.observe(RUN_COMMAND, &json!({}), "3 passed", false);
        t.observe(RUN_COMMAND, &json!({}), "3 passed", false);
        t.observe(RUN_COMMAND, &json!({}), "5 passed", false);
        assert!(t.gate().is_none(), "more tests passing is progress");
        assert_eq!(t.units(), (5.0, 0.0));
    }

    #[test]
    fn zero_disables() {
        let t = ProgressTracker::new(0);
        for _ in 0..50 {
            t.observe(READ_FILE, &json!({}), "", false);
        }
        assert!(t.gate().is_none());
    }
}

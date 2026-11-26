//! Hand-off cards and context parcels.
//!
//! When one agent hands work to another (an `@mention` cascade or a workflow
//! stage feeding the next), Ligus records a **card**: *what* the task is,
//! *where* it lives (branch, files) and *how good* the upstream result was
//! (measured: tests, proposal state, turns), so hand-offs are machine-readable
//! instead of buried in chat. An agent may attach a **context parcel** — key
//! files, decisions made, what was ruled out — with a `[[handoff: …]]`
//! directive; the receiver gets the parcel in its context and starts primed.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Cap on each parcel list and on the free-text fields, so a parcel stays a
/// compact summary rather than a second transcript.
pub const PARCEL_MAX_ITEMS: usize = 12;
pub const PARCEL_MAX_CHARS: usize = 400;

/// The measured quality of the work being handed on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffQuality {
    /// Last test command the upstream turn ran passed / failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<bool>,
    /// The upstream turn's proposal, if it filed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<Uuid>,
    /// That proposal's status (`open`, `approved`, …).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proposal_status: String,
    /// Turns spent upstream on this thread since the last human message.
    #[serde(default)]
    pub turns: u32,
    /// Estimated upstream cost (USD), when the runtime has a price.
    #[serde(default)]
    pub cost_usd: f64,
    /// The upstream turn failed.
    #[serde(default)]
    pub error: bool,
}

/// A compact summary handed to the receiver ("scent" of the work so far).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextParcel {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ruled_out: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

fn clip(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= PARCEL_MAX_CHARS {
        t.to_string()
    } else {
        let mut out: String = t.chars().take(PARCEL_MAX_CHARS).collect();
        out.push('…');
        out
    }
}

fn clip_list(v: &[String]) -> Vec<String> {
    v.iter().map(|s| clip(s)).filter(|s| !s.is_empty()).take(PARCEL_MAX_ITEMS).collect()
}

impl ContextParcel {
    pub fn is_empty(&self) -> bool {
        self.key_files.is_empty() && self.decisions.is_empty() && self.ruled_out.is_empty() && self.notes.trim().is_empty()
    }

    /// Bounded copy: trims, caps list lengths and text sizes.
    pub fn bounded(&self) -> Self {
        Self {
            key_files: clip_list(&self.key_files),
            decisions: clip_list(&self.decisions),
            ruled_out: clip_list(&self.ruled_out),
            notes: clip(&self.notes),
        }
    }

    /// The parcel as the receiver sees it in its context.
    pub fn render(&self, from: &str) -> String {
        let mut out = format!("Context parcel from {from} (their summary of the work so far):\n");
        let section = |out: &mut String, title: &str, items: &[String]| {
            if !items.is_empty() {
                out.push_str(title);
                out.push('\n');
                for i in items {
                    out.push_str("- ");
                    out.push_str(i);
                    out.push('\n');
                }
            }
        };
        section(&mut out, "Key files:", &self.key_files);
        section(&mut out, "Decisions made:", &self.decisions);
        section(&mut out, "Ruled out:", &self.ruled_out);
        if !self.notes.trim().is_empty() {
            out.push_str("Notes: ");
            out.push_str(self.notes.trim());
            out.push('\n');
        }
        out
    }
}

/// A structured hand-off between two agents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffCard {
    pub id: Uuid,
    /// Agent key of the sender and the receiver ([`super::agent_key`]).
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub from_name: String,
    #[serde(default)]
    pub to_name: String,
    /// The message that handed the work on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    /// What: a one-line task summary.
    pub task: String,
    pub task_class: String,
    /// Where: the chat's branch, if it has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    /// Where: files involved (the upstream proposal's changed files).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// How good.
    #[serde(default)]
    pub quality: HandoffQuality,
    /// `mention` or `stage`.
    #[serde(default)]
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parcel: Option<ContextParcel>,
    pub at_ms: i64,
}

impl HandoffCard {
    #[cfg(test)]
    pub(crate) fn example(from: &str, to: &str, at_ms: i64) -> Self {
        HandoffCard {
            id: Uuid::new_v4(),
            from: from.into(),
            to: to.into(),
            from_name: from.into(),
            to_name: to.into(),
            message_id: None,
            task: "task".into(),
            task_class: "general".into(),
            branch: String::new(),
            files: vec![],
            quality: HandoffQuality::default(),
            via: "mention".into(),
            parcel: None,
            at_ms,
        }
    }

    /// The card as the receiver sees it in its context.
    pub fn render(&self) -> String {
        let mut out = format!("Hand-off from {}: {}\n", self.from_name, self.task);
        if !self.branch.is_empty() {
            out.push_str(&format!("Branch: {}\n", self.branch));
        }
        if !self.files.is_empty() {
            let shown: Vec<&str> = self.files.iter().take(PARCEL_MAX_ITEMS).map(String::as_str).collect();
            out.push_str(&format!("Files: {}\n", shown.join(", ")));
        }
        let q = &self.quality;
        let mut facts = Vec::new();
        if let Some(t) = q.tests_passed {
            facts.push(if t { "tests passed".to_string() } else { "tests failing".to_string() });
        }
        if !q.proposal_status.is_empty() {
            facts.push(format!("proposal {}", q.proposal_status));
        }
        if q.error {
            facts.push("the previous turn errored".into());
        }
        if !facts.is_empty() {
            out.push_str(&format!("Status: {}\n", facts.join("; ")));
        }
        if let Some(p) = self.parcel.as_ref().filter(|p| !p.is_empty()) {
            out.push('\n');
            out.push_str(&p.render(&self.from_name));
        }
        out
    }
}

/// One-line task summary from the handing-off message: the sentence that
/// mentions the receiver, else the first line.
pub fn task_line(body: &str, to_name: &str) -> String {
    let needle = format!("@{}", to_name.to_lowercase());
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| l.to_lowercase().contains(&needle))
        .or_else(|| body.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    let mut out: String = line.chars().take(160).collect();
    if line.chars().count() > 160 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parcel_is_bounded() {
        let p = ContextParcel {
            key_files: (0..40).map(|i| format!("f{i}.rs")).collect(),
            decisions: vec!["x".repeat(1000)],
            ruled_out: vec!["  ".into()],
            notes: "n".into(),
        }
        .bounded();
        assert_eq!(p.key_files.len(), PARCEL_MAX_ITEMS);
        assert_eq!(p.decisions[0].chars().count(), PARCEL_MAX_CHARS + 1);
        assert!(p.ruled_out.is_empty());
    }

    #[test]
    fn card_renders_what_where_how_good_and_the_parcel() {
        let mut c = HandoffCard::example("a", "b", 0);
        c.from_name = "coder".into();
        c.task = "please review the parser change".into();
        c.branch = "ligus/chat/1".into();
        c.files = vec!["src/parser.rs".into()];
        c.quality.tests_passed = Some(true);
        c.quality.proposal_status = "open".into();
        c.parcel = Some(ContextParcel {
            key_files: vec!["src/parser.rs".into()],
            decisions: vec!["kept the old API".into()],
            ruled_out: vec!["a regex rewrite".into()],
            notes: String::new(),
        });
        let r = c.render();
        assert!(r.contains("Hand-off from coder: please review"));
        assert!(r.contains("Branch: ligus/chat/1"));
        assert!(r.contains("Files: src/parser.rs"));
        assert!(r.contains("tests passed; proposal open"));
        assert!(r.contains("Ruled out:\n- a regex rewrite"));
    }

    #[test]
    fn task_line_prefers_the_mentioning_sentence() {
        let body = "I changed the parser.\n@Reviewer can you check the edge cases?\nThanks";
        assert_eq!(task_line(body, "reviewer"), "@Reviewer can you check the edge cases?");
        assert_eq!(task_line("Only line", "x"), "Only line");
    }
}

//! Workspace marks: typed notes agents (and people) leave on the workspace
//! itself rather than in chat — a **claim** ("I'm taking this"), an
//! **under-construction** lock on a file or line range, a **needs-review**
//! marker. Other agents read the active marks in their context on their own
//! schedule, which coordinates work without agent-to-agent mention traffic.
//! Marks expire (claims and locks), so an abandoned one doesn't block forever.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::keys::normalize_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarkKind {
    /// "I'm working on this" — a file, a range or a task-board item.
    Claim,
    /// A lock: don't edit this file / range until it clears.
    UnderConstruction,
    /// Someone should review this.
    NeedsReview,
}

impl MarkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MarkKind::Claim => "claim",
            MarkKind::UnderConstruction => "under construction",
            MarkKind::NeedsReview => "needs review",
        }
    }

    /// Default lifetime: claims 4 h, locks 1 h, review markers until cleared.
    pub fn default_ttl_ms(self) -> Option<i64> {
        match self {
            MarkKind::Claim => Some(4 * 3_600_000),
            MarkKind::UnderConstruction => Some(3_600_000),
            MarkKind::NeedsReview => None,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_lowercase()
            .replace(['-', '_', ' '], "")
            .as_str()
        {
            "claim" | "todo" => Some(MarkKind::Claim),
            "underconstruction" | "lock" | "wip" => Some(MarkKind::UnderConstruction),
            "needsreview" | "review" => Some(MarkKind::NeedsReview),
            _ => None,
        }
    }
}

/// A mark on a file (optionally a line range) or on a task-board item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMark {
    pub id: Uuid,
    pub kind: MarkKind,
    /// Workspace-relative path; empty for a task mark.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Inclusive line range within `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<(u32, u32)>,
    /// Task-board item id, for a claim on board work.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    #[serde(default)]
    pub note: String,
    /// Agent key that left it, or empty for a person.
    #[serde(default)]
    pub agent: String,
    /// Display name of whoever left it.
    pub by: String,
    /// The chat it was left from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    pub at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<i64>,
}

impl WorkspaceMark {
    pub fn active(&self, now_ms: i64) -> bool {
        self.expires_ms.is_none_or(|e| now_ms < e)
    }

    /// Whether this mark covers `path` (and, if given, overlaps `lines`).
    pub fn covers(&self, path: &str, lines: Option<(u32, u32)>) -> bool {
        if self.path.is_empty() {
            return false;
        }
        let mine = normalize_path(&self.path);
        let theirs = normalize_path(path);
        let same = mine == theirs || (mine.ends_with('/') && theirs.starts_with(&mine));
        if !same {
            return false;
        }
        match (self.lines, lines) {
            (Some((a, b)), Some((c, d))) => a <= d && c <= b,
            _ => true,
        }
    }

    /// One line for an agent's context.
    pub fn describe(&self) -> String {
        let target = if !self.task_id.is_empty() {
            format!("task {}", self.task_id)
        } else {
            match self.lines {
                Some((a, b)) => format!("{}:{a}-{b}", self.path),
                None => self.path.clone(),
            }
        };
        let note = if self.note.trim().is_empty() {
            String::new()
        } else {
            format!(" — {}", self.note.trim())
        };
        format!("[{}] {target} by {}{note}", self.kind.as_str(), self.by)
    }
}

/// Active marks, newest first.
pub fn active<'a>(
    marks: impl IntoIterator<Item = &'a WorkspaceMark>,
    now_ms: i64,
) -> Vec<&'a WorkspaceMark> {
    let mut v: Vec<_> = marks.into_iter().filter(|m| m.active(now_ms)).collect();
    v.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then(a.id.cmp(&b.id)));
    v
}

/// Locks (and claims) held by someone other than `agent` over any of `files`.
pub fn conflicts<'a>(
    marks: impl IntoIterator<Item = &'a WorkspaceMark>,
    files: &[String],
    agent: &str,
    now_ms: i64,
) -> Vec<&'a WorkspaceMark> {
    active(marks, now_ms)
        .into_iter()
        .filter(|m| matches!(m.kind, MarkKind::UnderConstruction | MarkKind::Claim))
        .filter(|m| m.agent != agent)
        .filter(|m| files.iter().any(|f| m.covers(f, None)))
        .collect()
}

/// The marks block put in an agent's context: what others are working on and
/// what is locked. At most `max` lines.
pub fn context_block<'a>(
    marks: impl IntoIterator<Item = &'a WorkspaceMark>,
    now_ms: i64,
    max: usize,
) -> Option<String> {
    let act = active(marks, now_ms);
    if act.is_empty() {
        return None;
    }
    let mut out = String::from(
        "Workspace marks (left by agents and people; respect locks, don't redo claimed work, \
         and leave your own with [[mark: {\"kind\":\"claim|lock|review\",\"path\":\"…\",\"note\":\"…\"}]] \
         or clear one with [[unmark: {\"path\":\"…\"}]]):\n",
    );
    for m in act.iter().take(max) {
        out.push_str("- ");
        out.push_str(&m.describe());
        out.push('\n');
    }
    if act.len() > max {
        out.push_str(&format!("- … and {} more\n", act.len() - max));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(kind: MarkKind, path: &str, agent: &str, at: i64) -> WorkspaceMark {
        WorkspaceMark {
            id: Uuid::new_v4(),
            kind,
            path: path.into(),
            lines: None,
            task_id: String::new(),
            note: String::new(),
            agent: agent.into(),
            by: agent.into(),
            session_id: None,
            at_ms: at,
            expires_ms: kind.default_ttl_ms().map(|t| at + t),
        }
    }

    #[test]
    fn expiry_and_coverage() {
        let m = mark(MarkKind::UnderConstruction, "src/a.rs", "x", 0);
        assert!(m.active(10));
        assert!(!m.active(3_600_000));
        assert!(m.covers("./src/a.rs", None));
        assert!(!m.covers("src/b.rs", None));
        let mut r = m.clone();
        r.lines = Some((10, 20));
        assert!(r.covers("src/a.rs", Some((15, 30))));
        assert!(!r.covers("src/a.rs", Some((21, 30))));
        let mut dir = m.clone();
        dir.path = "src/".into();
        assert!(dir.covers("src/deep/x.rs", None));
        assert!(mark(MarkKind::NeedsReview, "a", "x", 0).active(i64::MAX));
    }

    #[test]
    fn conflicts_skip_own_marks_and_review_markers() {
        let marks = [
            mark(MarkKind::UnderConstruction, "src/a.rs", "other", 0),
            mark(MarkKind::UnderConstruction, "src/b.rs", "me", 0),
            mark(MarkKind::NeedsReview, "src/a.rs", "other", 0),
        ];
        let c = conflicts(&marks, &["src/a.rs".into(), "src/b.rs".into()], "me", 1);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].agent, "other");
    }

    #[test]
    fn context_block_lists_active_marks() {
        let marks = [
            mark(MarkKind::Claim, "src/a.rs", "coder", 5),
            mark(MarkKind::Claim, "old.rs", "coder", -5 * 3_600_000),
        ];
        let b = context_block(&marks, 10, 10).unwrap();
        assert!(b.contains("[claim] src/a.rs by coder"));
        assert!(!b.contains("old.rs"));
        assert!(context_block(&marks[1..], 10, 10).is_none());
    }

    #[test]
    fn kinds_parse_loosely() {
        assert_eq!(MarkKind::parse("lock"), Some(MarkKind::UnderConstruction));
        assert_eq!(
            MarkKind::parse("Under-Construction"),
            Some(MarkKind::UnderConstruction)
        );
        assert_eq!(MarkKind::parse("needs_review"), Some(MarkKind::NeedsReview));
        assert_eq!(MarkKind::parse("TODO"), Some(MarkKind::Claim));
        assert_eq!(MarkKind::parse("nope"), None);
    }
}

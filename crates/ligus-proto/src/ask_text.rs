//! Agent-written text on a decision card (`docs/design/agent-questions.md`
//! 6.4). A question and its options come from an agent, maybe a
//! prompt-injected one: they are shown as plain text (never Markdown, never
//! a link), cleaned of the characters that let text look like something
//! else, and an option may not pose as one of Ligus's own controls. The host
//! cleans before it posts, and every reader cleans again, so a crafted
//! record can't skip it.

use crate::names::is_hidden_name_char;

/// The longest question a v2 card takes (characters, after cleaning).
pub const MAX_QUESTION_CHARS: usize = 500;
/// The longest option (characters, after cleaning).
pub const MAX_OPTION_CHARS: usize = 60;
/// The longest free-text answer.
pub const MAX_TEXT_ANSWER_CHARS: usize = 1000;
/// Options on a v2 card.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 5;
/// Line breaks a question keeps; later ones become spaces.
pub const MAX_QUESTION_LINE_BREAKS: usize = 8;

/// Ligus's own words for a yes/no card (never the agent's).
pub const YES_NO: [&str; 2] = ["Yes", "No"];
/// Ligus's labels for a grant, by option index: 0 allows, 1 denies.
pub const GRANT_LABELS: [&str; 2] = ["Allow once", "Deny"];
/// The same, when the agent offered only a permission for the rest of its
/// session.
pub const STANDING_GRANT_LABELS: [&str; 2] = ["Allow for the rest of the session", "Deny"];
/// "Not now".
pub const NOT_NOW: &str = "Not now";

/// Labels of Ligus's own controls: an agent's option may not equal one
/// (case-insensitively, after cleaning).
pub const RESERVED_LABELS: [&str; 7] = [
    "Allow once",
    "Allow for the rest of the session",
    "Approve",
    "Approve & apply",
    "Deny",
    "Not now",
    "Reject",
];

/// A question as a card shows it: hidden and control characters removed
/// (tabs become spaces), at most [`MAX_QUESTION_LINE_BREAKS`] line breaks,
/// runs of spaces collapsed, each line trimmed, blank lines at the ends
/// dropped. Not cut: the caller bounds it.
pub fn clean_question(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        lines.push(clean_line(line));
    }
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    // At most two blank lines in a row, then at most N breaks in all.
    let mut out = String::new();
    let mut breaks = 0usize;
    let mut blank_run = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if l.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        if i > 0 {
            if breaks < MAX_QUESTION_LINE_BREAKS {
                out.push('\n');
                breaks += 1;
            } else if !out.ends_with(' ') && !l.is_empty() {
                out.push(' ');
            }
        }
        out.push_str(l);
    }
    out
}

/// An option as a card shows it: one line (newlines and tabs become a
/// space), hidden characters removed, spaces collapsed, trimmed.
pub fn clean_option(raw: &str) -> String {
    clean_line(raw)
}

fn clean_line(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut space = false;
    for c in raw.chars() {
        let c = if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c };
        if is_hidden_name_char(c) {
            continue;
        }
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// `s` cut to `max` characters, with an ellipsis when cut.
pub fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// Whether `option` (cleaned) poses as one of Ligus's controls.
pub fn is_reserved(option: &str) -> bool {
    let o = clean_option(option);
    RESERVED_LABELS.iter().any(|r| r.eq_ignore_ascii_case(&o))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_zero_width_and_controls_are_removed() {
        assert_eq!(clean_option("Ap\u{202E}prove\u{200B} it\u{0007}"), "Approve it");
        assert_eq!(clean_question("a\u{2066}b\u{2069}\u{FEFF}c"), "abc");
    }

    #[test]
    fn options_are_one_line() {
        assert_eq!(clean_option("  Post\ngres\t  now  "), "Post gres now");
    }

    #[test]
    fn a_question_keeps_at_most_eight_line_breaks() {
        let q = (0..20).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        let c = clean_question(&q);
        assert_eq!(c.matches('\n').count(), MAX_QUESTION_LINE_BREAKS);
        assert!(c.ends_with("l8 l9 l10 l11 l12 l13 l14 l15 l16 l17 l18 l19"));
        assert_eq!(clean_question("\n\n a \n\n\n\n b \n"), "a\n\nb");
    }

    #[test]
    fn control_labels_are_reserved() {
        for r in ["allow ONCE", "Deny", " Not\u{200B} now ", "approve & apply"] {
            assert!(is_reserved(r), "{r}");
        }
        assert!(!is_reserved("Postgres") && !is_reserved("Yes"));
    }

    #[test]
    fn cut_marks_what_it_drops() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
    }
}

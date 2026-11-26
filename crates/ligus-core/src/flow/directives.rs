//! Flow directives in agent replies. Any runtime can emit them as plain text —
//! a CLI agent, an HTTP model, an API turn with MCP tools — so they need no
//! tool slot:
//!
//! - `[[handoff: {"to":"reviewer","keyFiles":[…],"decisions":[…],"ruledOut":[…],"notes":"…"}]]`
//!   attaches a context parcel to the hand-off this reply makes (`to` is
//!   optional; the reply's `@mention` names the receiver).
//! - `[[mark: {"kind":"claim|lock|review","path":"src/x.rs","lines":[10,40],"note":"…"}]]`
//!   leaves a workspace mark.
//! - `[[unmark: {"path":"src/x.rs"}]]` (or `{"id":"…"}`, or `{"kind":"claim"}`)
//!   clears the agent's own marks on that path.
//!
//! Directives are stripped from the visible reply. Malformed ones are dropped
//! (and still stripped).

use serde::Deserialize;
use uuid::Uuid;

use super::handoff::ContextParcel;
use super::marks::MarkKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParcelDirective {
    pub to: Option<String>,
    pub parcel: ContextParcel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkDirective {
    pub kind: MarkKind,
    pub path: String,
    pub lines: Option<(u32, u32)>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnmarkDirective {
    pub id: Option<Uuid>,
    pub path: Option<String>,
    pub kind: Option<MarkKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlowDirectives {
    pub parcels: Vec<ParcelDirective>,
    pub marks: Vec<MarkDirective>,
    pub unmarks: Vec<UnmarkDirective>,
    /// The reply with the directives removed.
    pub cleaned: String,
}

impl FlowDirectives {
    pub fn is_empty(&self) -> bool {
        self.parcels.is_empty() && self.marks.is_empty() && self.unmarks.is_empty()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawParcel {
    #[serde(default)]
    to: Option<String>,
    #[serde(default, alias = "files")]
    key_files: Vec<String>,
    #[serde(default)]
    decisions: Vec<String>,
    #[serde(default, alias = "ruled_out")]
    ruled_out: Vec<String>,
    #[serde(default)]
    notes: String,
}

#[derive(Deserialize)]
struct RawMark {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    lines: Option<(u32, u32)>,
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
struct RawUnmark {
    #[serde(default)]
    id: Option<Uuid>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

/// Length of the balanced JSON object starting at `s[0] == '{'`, honouring
/// strings and escapes, or `None` if it never closes.
fn json_object_len(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if in_str {
            match c {
                _ if esc => esc = false,
                '\\' => esc = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

const NAMES: [&str; 3] = ["handoff:", "mark:", "unmark:"];

/// Parse and strip flow directives.
pub fn parse(body: &str) -> FlowDirectives {
    let mut out = FlowDirectives::default();
    let mut cleaned = String::with_capacity(body.len());
    let mut matched = false;
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if let Some(inner) = rest.strip_prefix("[[") {
            let trimmed = inner.trim_start();
            let lead = inner.len() - trimmed.len();
            if let Some(name) = NAMES.iter().find(|n| trimmed.starts_with(*n)) {
                let after_name = &trimmed[name.len()..];
                let ws = after_name.len() - after_name.trim_start().len();
                let json_at = 2 + lead + name.len() + ws;
                let json = &rest[json_at..];
                if json.starts_with('{') {
                    if let Some(len) = json_object_len(json) {
                        let tail = &json[len..];
                        let tail_ws = tail.len() - tail.trim_start().len();
                        if tail[tail_ws..].starts_with("]]") {
                            take(name, &json[..len], &mut out);
                            matched = true;
                            i += json_at + len + tail_ws + 2;
                            continue;
                        }
                    }
                }
            }
        }
        let c = rest.chars().next().expect("non-empty");
        cleaned.push(c);
        i += c.len_utf8();
    }
    out.cleaned = if matched {
        tidy(&cleaned)
    } else {
        body.to_string()
    };
    out
}

/// Collapse the blank lines a removed directive leaves behind.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim().to_string()
}

fn take(name: &str, json: &str, out: &mut FlowDirectives) {
    match name {
        "handoff:" => {
            if let Ok(r) = serde_json::from_str::<RawParcel>(json) {
                let parcel = ContextParcel {
                    key_files: r.key_files,
                    decisions: r.decisions,
                    ruled_out: r.ruled_out,
                    notes: r.notes,
                }
                .bounded();
                if !parcel.is_empty() {
                    out.parcels.push(ParcelDirective {
                        to: r.to.map(|t| t.trim_start_matches('@').to_string()),
                        parcel,
                    });
                }
            }
        }
        "mark:" => {
            if let Ok(r) = serde_json::from_str::<RawMark>(json) {
                if let Some(kind) = MarkKind::parse(&r.kind) {
                    let path = super::keys::normalize_path(r.path.trim());
                    if !path.is_empty() && !path.contains("..") {
                        let lines = r.lines.map(|(a, b)| (a.min(b), a.max(b)));
                        out.marks.push(MarkDirective {
                            kind,
                            path,
                            lines,
                            note: r.note.chars().take(200).collect(),
                        });
                    }
                }
            }
        }
        "unmark:" => {
            if let Ok(r) = serde_json::from_str::<RawUnmark>(json) {
                let u = UnmarkDirective {
                    id: r.id,
                    path: r
                        .path
                        .map(|p| super::keys::normalize_path(p.trim()))
                        .filter(|p| !p.is_empty()),
                    kind: r.kind.as_deref().and_then(MarkKind::parse),
                };
                if u != UnmarkDirective::default() {
                    out.unmarks.push(u);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_strips_directives() {
        let body = "Done with the parser.\n\n[[handoff: {\"to\":\"@reviewer\",\"keyFiles\":[\"src/p.rs\"],\"decisions\":[\"keep the API\"],\"ruledOut\":[\"regex ]] rewrite\"]}]]\n\n@reviewer please check.\n[[mark: {\"kind\":\"lock\",\"path\":\"./src/p.rs\",\"lines\":[40,10]}]]\n[[unmark: {\"path\":\"src/old.rs\"}]]";
        let d = parse(body);
        assert_eq!(d.parcels.len(), 1);
        assert_eq!(d.parcels[0].to.as_deref(), Some("reviewer"));
        assert_eq!(d.parcels[0].parcel.ruled_out, vec!["regex ]] rewrite"]);
        assert_eq!(
            d.marks,
            vec![MarkDirective {
                kind: MarkKind::UnderConstruction,
                path: "src/p.rs".into(),
                lines: Some((10, 40)),
                note: String::new()
            }]
        );
        assert_eq!(d.unmarks[0].path.as_deref(), Some("src/old.rs"));
        assert_eq!(
            d.cleaned,
            "Done with the parser.\n\n@reviewer please check."
        );
    }

    #[test]
    fn leaves_other_directives_and_text_alone() {
        let body = "Hi [[react: 👍]] and [[propose: {\"title\":\"x\"}]]";
        let d = parse(body);
        assert!(d.is_empty());
        assert_eq!(d.cleaned, body);
    }

    #[test]
    fn malformed_and_unsafe_ones_are_dropped_but_stripped() {
        let d = parse("a [[mark: {\"kind\":\"nope\",\"path\":\"x\"}]] b [[mark: {\"kind\":\"claim\",\"path\":\"../etc/passwd\"}]] c [[handoff: {}]]");
        assert!(d.is_empty());
        assert_eq!(d.cleaned, "a  b  c");
        let d = parse("a [[mark: {\"kind\":\"claim\",\"path\":\"ok.rs\"}]] b [[mark: {\"kind\":\"nope\",\"path\":\"x\"}]]");
        assert_eq!(d.marks.len(), 1);
        assert_eq!(d.cleaned, "a  b");
    }

    #[test]
    fn unclosed_directive_is_text() {
        let d = parse("[[mark: {\"kind\":\"claim\"");
        assert!(d.is_empty());
    }
}

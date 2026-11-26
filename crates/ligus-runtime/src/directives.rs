//! Inline agent directives — ported from `parseReactionDirectives` /
//! `finalizeReplyApplyingReactions` in `PrototypeStore.swift`.
//!
//! Agents emit shorthand in their replies:
//! - `[[react: 👍]]` — react to their own message
//! - `[[vote: 👍 👎]]` — prepopulate clickable reaction chips for others
//! - `[[workflow: { …json… }]]` — author a workflow definition (validated and
//!   saved by the app layer; inert until a human runs it)
//! - `[[propose: { …json… }]]` — author an action proposal saved for human
//!   review (approved via quorum in the Review pane; never auto-executed)
//! - `[[alternatives: { …json… }]]` — open a proposal set: one question,
//!   several competing alternatives for scouts to assess
//! - `[[assess: { …json… }]]` — a scout's score for one alternative of a set
//!
//! Directives are stripped from the visible body. Pure + unit-tested.

use ligus_core::ProposalKind;
use serde::Deserialize;

/// A parsed `[[propose: …]]` directive. The app layer turns this into an
/// `ActionProposal` authored by the responding agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedProposal {
    pub title: String,
    pub body: String,
    pub kind: ProposalKind,
    pub required_approvals: u32,
}

/// Shape of the JSON body of a `[[propose: …]]` directive. `title` is required;
/// everything else defaults. A missing/malformed block is skipped (see
/// [`parse_reply_directives`]).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProposal {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    kind: Option<ProposalKind>,
    #[serde(default)]
    required_approvals: Option<u32>,
}

/// One option of an `[[alternatives: …]]` directive.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedAlternative {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub kind: Option<ProposalKind>,
}

/// A parsed `[[alternatives: …]]` directive: the app files each option as a
/// proposal and opens a proposal set over them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedAlternatives {
    pub question: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub task_class: Option<String>,
    /// Requested agent quorum `k`; clamped by the fold.
    #[serde(default)]
    pub agent_quorum: Option<u32>,
    #[serde(default, alias = "alternatives")]
    pub options: Vec<ParsedAlternative>,
}

/// A parsed `[[assess: …]]` directive. `alternative` is the proposal id the
/// scout was asked to rate (a letter like "B" is also accepted and resolved
/// against the set by the app). Who the scout is comes from the turn, never
/// from this payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedAssessment {
    pub alternative: String,
    pub score: u32,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplyDirectives {
    /// Emoji to seed as reactions on the reply, in order, deduped.
    pub emojis: Vec<String>,
    /// Raw JSON payloads of `[[workflow: …]]` directives, in order. Parsing/
    /// validation happens in the app layer (which owns the save path).
    pub workflows: Vec<String>,
    /// Parsed `[[propose: …]]` directives, in order. Malformed blocks are
    /// skipped (lenient — a bad proposal doesn't fail the whole reply).
    pub proposals: Vec<ParsedProposal>,
    /// Parsed `[[alternatives: …]]` directives (at least two options each).
    pub alternatives: Vec<ParsedAlternatives>,
    /// Parsed `[[assess: …]]` directives.
    pub assessments: Vec<ParsedAssessment>,
    /// The body with the directive markers removed.
    pub cleaned: String,
}

/// Parse `[[react: …]]` / `[[vote: …]]` / `[[workflow: …]]` directives out of
/// an agent reply.
pub fn parse_reply_directives(body: &str) -> ReplyDirectives {
    let mut emojis: Vec<String> = Vec::new();
    let mut workflows: Vec<String> = Vec::new();
    let mut proposals: Vec<ParsedProposal> = Vec::new();
    let mut alternatives: Vec<ParsedAlternatives> = Vec::new();
    let mut assessments: Vec<ParsedAssessment> = Vec::new();
    let mut cleaned = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < body.len() {
        if body[i..].starts_with("[[") {
            // Workflow payloads are JSON, which can legally contain "]]"
            // inside strings — scan the balanced object instead of searching
            // for the closer.
            if let Some(rest) = body[i + 2..].strip_prefix("workflow:") {
                let json_start = i + 2 + "workflow:".len() + leading_ws(rest);
                if body[json_start..].starts_with('{') {
                    if let Some(obj_len) = scan_json_object(&body[json_start..]) {
                        let after = json_start + obj_len;
                        let close = after + leading_ws(&body[after..]);
                        if body[close..].starts_with("]]") {
                            workflows.push(body[json_start..json_start + obj_len].to_string());
                            i = close + 2;
                            continue;
                        }
                    }
                }
            }
            // Proposal payloads are JSON too — scan the balanced object so a
            // body containing "]]" is handled. A malformed/incomplete block is
            // skipped (still stripped) rather than failing the reply.
            if let Some(rest) = body[i + 2..].strip_prefix("propose:") {
                let json_start = i + 2 + "propose:".len() + leading_ws(rest);
                if body[json_start..].starts_with('{') {
                    if let Some(obj_len) = scan_json_object(&body[json_start..]) {
                        let after = json_start + obj_len;
                        let close = after + leading_ws(&body[after..]);
                        if body[close..].starts_with("]]") {
                            let json = &body[json_start..json_start + obj_len];
                            if let Ok(raw) = serde_json::from_str::<RawProposal>(json) {
                                if !raw.title.trim().is_empty() {
                                    proposals.push(ParsedProposal {
                                        title: raw.title,
                                        body: raw.body,
                                        kind: raw.kind.unwrap_or(ProposalKind::Decision),
                                        required_approvals: raw.required_approvals.unwrap_or(1),
                                    });
                                }
                            }
                            i = close + 2;
                            continue;
                        }
                    }
                }
            }
            // Proposal sets: `alternatives` opens one, `assess` scores an
            // option. Same balanced-JSON scan; a malformed block is stripped.
            if let Some((json, end)) = json_directive(body, i, "alternatives:") {
                if let Ok(raw) = serde_json::from_str::<ParsedAlternatives>(json) {
                    let options: Vec<ParsedAlternative> = raw
                        .options
                        .iter()
                        .filter(|o| !o.title.trim().is_empty())
                        .cloned()
                        .collect();
                    if !raw.question.trim().is_empty() && options.len() >= 2 {
                        alternatives.push(ParsedAlternatives { options, ..raw });
                    }
                }
                i = end;
                continue;
            }
            if let Some((json, end)) = json_directive(body, i, "assess:") {
                if let Ok(raw) = serde_json::from_str::<ParsedAssessment>(json) {
                    if !raw.alternative.trim().is_empty() {
                        assessments.push(raw);
                    }
                }
                i = end;
                continue;
            }
            if let Some(end_rel) = body[i..].find("]]") {
                let inner = &body[i + 2..i + end_rel];
                let lower = inner.trim_start();
                let payload = lower
                    .strip_prefix("react:")
                    .or_else(|| lower.strip_prefix("vote:"));
                if let Some(payload) = payload {
                    for tok in payload.split_whitespace() {
                        let tok = tok.to_string();
                        if !emojis.contains(&tok) {
                            emojis.push(tok);
                        }
                    }
                    i += end_rel + 2; // skip the whole directive
                    continue;
                }
            }
        }
        // copy one char (respecting UTF-8 boundaries)
        let ch_len = utf8_char_len(bytes[i]);
        cleaned.push_str(&body[i..i + ch_len]);
        i += ch_len;
    }
    ReplyDirectives {
        emojis,
        workflows,
        proposals,
        alternatives,
        assessments,
        cleaned: cleaned.trim().to_string(),
    }
}

/// A `[[<name> {json}]]` directive starting at `body[i..]` (which begins with
/// `[[`): the JSON object and the index just past the closing `]]`. `None`
/// when it isn't one or the JSON is unbalanced.
fn json_directive<'a>(body: &'a str, i: usize, name: &str) -> Option<(&'a str, usize)> {
    let rest = body[i + 2..].strip_prefix(name)?;
    let json_start = i + 2 + name.len() + leading_ws(rest);
    if !body[json_start..].starts_with('{') {
        return None;
    }
    let obj_len = scan_json_object(&body[json_start..])?;
    let after = json_start + obj_len;
    let close = after + leading_ws(&body[after..]);
    body[close..]
        .starts_with("]]")
        .then(|| (&body[json_start..after], close + 2))
}

fn leading_ws(s: &str) -> usize {
    s.len() - s.trim_start().len()
}

/// Byte length of the balanced JSON object starting at `s[0] == '{'`,
/// respecting strings and escapes. `None` if unbalanced.
fn scan_json_object(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (idx, c) in s.bytes().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(idx + 1);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_react_and_vote_and_strips() {
        let r = parse_reply_directives("Shipping it. [[vote: 👍 👎]] thoughts? [[react: 🎉]]");
        assert_eq!(r.emojis, vec!["👍", "👎", "🎉"]);
        assert_eq!(r.cleaned, "Shipping it.  thoughts?");
    }

    #[test]
    fn no_directives_passes_through() {
        let r = parse_reply_directives("just a normal reply");
        assert!(r.emojis.is_empty());
        assert!(r.workflows.is_empty());
        assert_eq!(r.cleaned, "just a normal reply");
    }

    #[test]
    fn dedups_repeated_emoji() {
        let r = parse_reply_directives("[[vote: 👍 👍 👎]]");
        assert_eq!(r.emojis, vec!["👍", "👎"]);
    }

    #[test]
    fn extracts_workflow_json_and_strips() {
        let body = "Here you go.\n[[workflow: {\"name\": \"Triage\", \"stages\": [{\"id\": \"scan\", \"prompt\": \"{{input}}\"}]}]]\nRun it from the pane.";
        let r = parse_reply_directives(body);
        assert_eq!(r.workflows.len(), 1);
        assert!(r.workflows[0].starts_with("{\"name\""));
        assert_eq!(r.cleaned, "Here you go.\n\nRun it from the pane.");
        // The payload parses as the JSON that was embedded.
        let v: serde_json::Value = serde_json::from_str(&r.workflows[0]).unwrap();
        assert_eq!(v["name"], "Triage");
    }

    #[test]
    fn workflow_json_may_contain_double_brackets_in_strings() {
        let body =
            r#"[[workflow: {"name": "x", "note": "arrays like [[1]] are fine", "stages": []}]]"#;
        let r = parse_reply_directives(body);
        assert_eq!(r.workflows.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&r.workflows[0]).unwrap();
        assert_eq!(v["note"], "arrays like [[1]] are fine");
        assert_eq!(r.cleaned, "");
    }

    #[test]
    fn malformed_workflow_directive_is_left_verbatim() {
        // Unbalanced JSON → not treated as a directive; text passes through so
        // the user can see what the agent attempted.
        let body = "[[workflow: {\"name\": \"broken\"]]";
        let r = parse_reply_directives(body);
        assert!(r.workflows.is_empty());
        assert_eq!(r.cleaned, body);
    }

    #[test]
    fn parses_wellformed_proposal_and_strips() {
        let body = "Proposing a change.\n[[propose: {\"title\": \"Bump timeout\", \"kind\": \"command\", \"body\": \"raise it to 30s\", \"requiredApprovals\": 2}]]\nLet me know.";
        let r = parse_reply_directives(body);
        assert_eq!(r.proposals.len(), 1);
        let p = &r.proposals[0];
        assert_eq!(p.title, "Bump timeout");
        assert_eq!(p.kind, ProposalKind::Command);
        assert_eq!(p.body, "raise it to 30s");
        assert_eq!(p.required_approvals, 2);
        assert_eq!(r.cleaned, "Proposing a change.\n\nLet me know.");
    }

    #[test]
    fn proposal_defaults_kind_and_approvals() {
        let r = parse_reply_directives(r#"[[propose: {"title": "Ship v2"}]]"#);
        assert_eq!(r.proposals.len(), 1);
        assert_eq!(r.proposals[0].kind, ProposalKind::Decision);
        assert_eq!(r.proposals[0].required_approvals, 1);
        assert_eq!(r.proposals[0].body, "");
        assert_eq!(r.cleaned, "");
    }

    #[test]
    fn proposal_body_may_contain_double_brackets() {
        let body = r#"[[propose: {"title": "Note", "body": "arrays like [[1]] are fine"}]]"#;
        let r = parse_reply_directives(body);
        assert_eq!(r.proposals.len(), 1);
        assert_eq!(r.proposals[0].body, "arrays like [[1]] are fine");
        assert_eq!(r.cleaned, "");
    }

    #[test]
    fn malformed_proposal_missing_title_is_skipped_not_fatal() {
        // Balanced JSON but no title → skipped (and stripped), reply survives.
        let body = "Here. [[propose: {\"kind\": \"decision\"}]] Done.";
        let r = parse_reply_directives(body);
        assert!(r.proposals.is_empty());
        assert_eq!(r.cleaned, "Here.  Done.");
    }

    #[test]
    fn no_proposal_directive_yields_empty() {
        let r = parse_reply_directives("just a normal reply");
        assert!(r.proposals.is_empty());
    }

    #[test]
    fn workflow_and_reactions_coexist() {
        let body = "Done. [[react: ✅]] [[workflow: {\"name\": \"w\", \"stages\": []}]]";
        let r = parse_reply_directives(body);
        assert_eq!(r.emojis, vec!["✅"]);
        assert_eq!(r.workflows.len(), 1);
        assert_eq!(r.cleaned, "Done.");
    }

    #[test]
    fn parses_alternatives_and_strips() {
        let body = r#"Three ways. [[alternatives: {"question": "Which cache?", "taskClass": "design", "agentQuorum": 2, "options": [{"title": "Redis", "body": "shared"}, {"title": "LRU", "kind": "command"}, {"title": ""}]}]] Pick one."#;
        let r = parse_reply_directives(body);
        assert_eq!(r.alternatives.len(), 1);
        let a = &r.alternatives[0];
        assert_eq!(a.question, "Which cache?");
        assert_eq!(a.task_class.as_deref(), Some("design"));
        assert_eq!(a.agent_quorum, Some(2));
        assert_eq!(a.options.len(), 2, "an untitled option is dropped");
        assert_eq!(a.options[1].kind, Some(ProposalKind::Command));
        assert_eq!(r.cleaned, "Three ways.  Pick one.");
    }

    #[test]
    fn alternatives_need_a_question_and_two_options() {
        let r = parse_reply_directives(
            r#"[[alternatives: {"question": "Q", "options": [{"title": "only"}]}]] x"#,
        );
        assert!(r.alternatives.is_empty());
        assert_eq!(r.cleaned, "x");
    }

    #[test]
    fn parses_assessment_and_strips() {
        let body = r#"Looked at it. [[assess: {"alternative": "B", "score": 82, "rationale": "fewer moving parts]] here"}]]"#;
        let r = parse_reply_directives(body);
        assert_eq!(r.assessments.len(), 1);
        assert_eq!(r.assessments[0].alternative, "B");
        assert_eq!(r.assessments[0].score, 82);
        assert!(r.assessments[0].rationale.contains("fewer moving parts"));
        assert_eq!(r.cleaned, "Looked at it.");
        // A negative or missing score is malformed: stripped, not recorded.
        let bad = parse_reply_directives(r#"[[assess: {"alternative": "B", "score": -3}]]"#);
        assert!(bad.assessments.is_empty());
    }
}

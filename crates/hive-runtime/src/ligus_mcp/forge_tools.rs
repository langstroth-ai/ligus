//! Read-only code-host tools for agents (IDENTITY-AND-FORGES 5.5):
//! `ligus_forge_list_prs`, `…_read_pr`, `…_list_issues`, `…_read_issue`,
//! `…_ci_status`, `…_read_file`.
//!
//! - They run in the host process on [`hive_forge::ForgeReader`], which has
//!   no write method: no fork, no change request, no push, no comment
//!   (`agent_tools_have_no_forge_writes`). The token never enters the
//!   sandbox; the agent sees only the results.
//! - `repo` is limited to the chat's bound repository and its upstream.
//!   An agent can't point the host at an arbitrary repository (or use the
//!   host's credential on someone else's private one).
//! - The egress tier gates them: a `Closed` chat (lockdown, dearth) gets
//!   none; an allow-listed chat only when its list names [`EGRESS_ID`];
//!   open egress, always. The host's request to the forge is network
//!   egress made on the agent's behalf, so the chat's envelope decides.
//! - Budgets: each call counts against `maxToolCalls` like every Ligus
//!   tool, and against `maxForgeCalls` (default 16 a turn).
//! - Results are data, not instructions ([`super::server::READ_HEADER`]),
//!   and bounded: lists ≤ 50 rows, bodies and files ≤ 64 KiB of text.

use std::sync::Arc;

use hive_core::sandbox::EgressPolicy;
use hive_forge::{ForgeError, ForgeReader, RepoRef, StateFilter};
use serde_json::{json, Value};

use super::server::{Outcome, ToolResult, READ_HEADER};

/// The id an egress allow-list names to let agents read the code host.
pub const EGRESS_ID: &str = "ligus.forge";

/// What a turn's code-host reads may reach: the reader for the chat's bound
/// repository, that repository, and its upstream for a fork.
#[derive(Clone)]
pub struct ForgeBinding {
    pub reader: Arc<dyn ForgeReader>,
    pub repo: RepoRef,
    pub upstream: Option<RepoRef>,
}

impl ForgeBinding {
    /// The repository a call names: `owner/name` equal to the bound repo or
    /// its upstream; empty = the upstream of a fork (where its pull requests
    /// open), else the bound repo.
    fn target(&self, raw: &str) -> Result<RepoRef, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(self.upstream.clone().unwrap_or_else(|| self.repo.clone()));
        }
        let allowed = std::iter::once(&self.repo).chain(self.upstream.iter());
        allowed
            .into_iter()
            .find(|r| r.full_name().eq_ignore_ascii_case(raw))
            .cloned()
            .ok_or_else(|| format!("only this chat's repository ({}) or its upstream can be read", self.repo.full_name()))
    }
}

/// Whether the egress envelope lets this turn read the code host.
pub fn egress_allows(egress: &EgressPolicy) -> bool {
    match egress {
        EgressPolicy::Open => true,
        EgressPolicy::AllowList { mcp_servers } => mcp_servers.iter().any(|s| s == EGRESS_ID),
        EgressPolicy::Closed => false,
    }
}

fn denied(msg: impl Into<String>) -> ToolResult {
    ToolResult { text: json!({"error": "denied", "message": msg.into()}).to_string(), is_error: true, outcome: Outcome::Denied }
}

fn failed(e: ForgeError) -> ToolResult {
    let outcome = match e {
        ForgeError::Unauthorized(_) | ForgeError::NotSignedIn(_) => Outcome::Denied,
        _ => Outcome::Error,
    };
    ToolResult { text: json!({"error": "failed", "message": e.to_string()}).to_string(), is_error: true, outcome }
}

fn ok(v: Value) -> ToolResult {
    ToolResult { text: v.to_string(), is_error: false, outcome: Outcome::Ok }
}

/// Run one code-host read. The caller has authorized the turn, charged the
/// budgets and checked the egress tier.
pub async fn call(b: &ForgeBinding, tool: &str, args: &Value) -> ToolResult {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let repo = match b.target(&s("repo")) {
        Ok(r) => r,
        Err(m) => return denied(m),
    };
    let state = match StateFilter::parse(&s("state")) {
        Some(st) => st,
        None => return denied("state is open, closed or all"),
    };
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 50) as u32;
    let number = s("number");
    if matches!(tool, "ligus_forge_read_pr" | "ligus_forge_read_issue") && (number.is_empty() || number.len() > 20 || !number.bytes().all(|c| c.is_ascii_digit())) {
        return denied("number is the pull request or issue number");
    }
    let head = |extra: Value| {
        let mut v = json!({"header": READ_HEADER, "forge": b.reader.id().as_str(), "repo": repo.full_name()});
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        v
    };
    let r = &b.reader;
    match tool {
        "ligus_forge_list_prs" => match r.list_change_requests(&repo, state, limit).await {
            Ok(list) => ok(head(json!({"changeRequests": list.into_iter().take(50).map(|mut c| { c.body = c.body.chars().take(500).collect(); c }).collect::<Vec<_>>()}))),
            Err(e) => failed(e),
        },
        "ligus_forge_read_pr" => {
            let cr = match r.change_request(&repo, &number).await {
                Ok(c) => c,
                Err(e) => return failed(e),
            };
            let comments = r.review_comments(&repo, &number).await.map(|c| c.into_iter().take(100).collect::<Vec<_>>());
            let ci = match cr.head_sha.as_deref() {
                Some(sha) => r.ci_status(&repo, sha).await.ok(),
                None => None,
            };
            ok(head(json!({"changeRequest": cr, "reviewComments": comments.unwrap_or_default(), "ci": ci})))
        }
        "ligus_forge_list_issues" => match r.issues(&repo, state, limit).await {
            Ok(list) => ok(head(json!({"issues": list.into_iter().take(50).map(|mut i| { i.body = i.body.chars().take(500).collect(); i }).collect::<Vec<_>>()}))),
            Err(e) => failed(e),
        },
        "ligus_forge_read_issue" => match r.issue(&repo, &number).await {
            Ok(i) => ok(head(json!({"issue": i}))),
            Err(e) => failed(e),
        },
        "ligus_forge_ci_status" => {
            let git_ref = s("ref");
            if !hive_forge::spec::valid_git_ref(&git_ref) {
                return denied("ref is a branch, tag or commit");
            }
            match r.ci_status(&repo, &git_ref).await {
                Ok(c) => ok(head(json!({"ref": git_ref, "ci": c}))),
                Err(e) => failed(e),
            }
        }
        "ligus_forge_read_file" => {
            let path = s("path");
            let git_ref = Some(s("ref")).filter(|r| !r.is_empty()).unwrap_or_else(|| "HEAD".into());
            if !hive_forge::spec::valid_file_path(&path) || !hive_forge::spec::valid_git_ref(&git_ref) {
                return denied("path is a repository-relative file path and ref a branch, tag or commit");
            }
            match r.file_at(&repo, &path, &git_ref).await {
                Ok(f) => ok(head(json!({"file": f}))),
                Err(e) => failed(e),
            }
        }
        _ => denied("unknown code-host read"),
    }
}

/// Whether a tool name is one of these.
pub fn is_forge_tool(name: &str) -> bool {
    name.starts_with("ligus_forge_")
}

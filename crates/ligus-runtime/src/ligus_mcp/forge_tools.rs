//! Read-only code-host tools for agents (IDENTITY-AND-FORGES 5.5):
//! `ligus_forge_list_prs`, `…_read_pr`, `…_list_issues`, `…_read_issue`,
//! `…_ci_status`, `…_read_file`.
//!
//! - They run in the host process on [`ligus_forge::ForgeReader`], which has
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

use ligus_core::sandbox::EgressPolicy;
use ligus_forge::{ForgeError, ForgeReader, RepoRef, StateFilter};
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
            if !ligus_forge::spec::valid_git_ref(&git_ref) {
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
            if !ligus_forge::spec::valid_file_path(&path) || !ligus_forge::spec::valid_git_ref(&git_ref) {
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

/// A remote's URL exactly as configured (`remote.<name>.url`, the first one),
/// without git's `insteadOf`/`pushInsteadOf` rewriting. The binding names the
/// repository the person configured: a rewrite (in the checkout's own config,
/// which agents may have touched, or a global one) must not turn it into a
/// different repository the host's credential would then read.
pub fn configured_remote_url(dir: &std::path::Path, remote: &str) -> Option<String> {
    let o = ligus_core::process_util::git()
        .arg("-C")
        .arg(dir)
        .args(["config", "--get-all", &format!("remote.{remote}.url")])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

/// The read-only binding for a checkout: its configured `origin` on one of
/// `forges`, and `upstream` (when it names another repository on the same
/// forge).
pub fn binding_for_checkout(dir: &std::path::Path, forges: &[Arc<dyn ligus_forge::Forge>]) -> Option<ForgeBinding> {
    let origin = configured_remote_url(dir, "origin")?;
    for f in forges {
        if let Some(repo) = f.repo_of_remote(&origin) {
            let upstream = configured_remote_url(dir, "upstream").and_then(|u| f.repo_of_remote(&u)).filter(|u| !u.same(&repo));
            return Some(ForgeBinding { reader: Arc::new(ligus_forge::ReadOnly(f.clone())), repo, upstream });
        }
    }
    None
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let o = ligus_core::process_util::git().arg("-C").arg(dir).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    /// `url.<base>.insteadOf` rewrites what `git remote get-url` reports (and
    /// where fetches go); the binding reads the configured URL instead, so a
    /// rewrite can't point the agents' reads (on the host's credential) at
    /// another repository.
    #[test]
    fn binding_ignores_rewritten_remote() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        git(dir, &["init", "-q"]);
        git(dir, &["remote", "add", "origin", "https://github.com/me/app.git"]);
        git(dir, &["remote", "add", "upstream", "https://github.com/org/app.git"]);
        git(dir, &["config", "url.https://github.com/victim/.insteadOf", "https://github.com/me/"]);
        git(dir, &["config", "url.https://github.com/victim2/.pushInsteadOf", "https://github.com/me/"]);
        git(dir, &["config", "url.https://github.com/other/.insteadOf", "https://github.com/org/"]);
        let c = ligus_forge::Connection::new(ligus_forge::ForgeKind::GitHub, "https://github.com", ligus_forge::AuthSource::None).unwrap();
        let f = c.build(Arc::new(ligus_forge::tokens::Anonymous));
        let b = binding_for_checkout(dir, &[f]).expect("origin is on github.com");
        assert_eq!(b.repo.full_name(), "me/app");
        assert_eq!(b.upstream.map(|u| u.full_name()).as_deref(), Some("org/app"));
    }
}

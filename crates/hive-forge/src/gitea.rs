//! Forgejo, Gitea and Codeberg (one API, `{base}/api/v1`).
//!
//! - Tokens go as `Authorization: token <t>` (both document it:
//!   <https://forgejo.org/docs/latest/user/api-usage/>,
//!   <https://docs.gitea.com/development/api-usage>).
//! - Pull requests have no draft field: a draft is a `WIP:` title prefix,
//!   one of the default `WORK_IN_PROGRESS_PREFIXES` (`WIP:,[WIP]`).
//! - A cross-repo head is `owner:branch`; an existing PR answers 409
//!   ("pull request already exists for these targets").
//! - No device sign-in exists on these forges, and Forgejo's OAuth tokens
//!   carry no scopes: a scoped personal access token is the 2.0 path.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

use crate::http::{id_str, s, Api, AuthStyle};
use crate::tokens::CredentialSource;
use crate::*;

pub struct Gitea {
    id: ForgeId,
    web: String,
    api: Api,
}

/// The title of a work-in-progress pull request.
pub fn wip_title(title: &str) -> String {
    let t = title.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("wip:") || lower.starts_with("[wip]") {
        t.to_string()
    } else {
        format!("WIP: {t}")
    }
}

fn is_wip(title: &str) -> bool {
    let l = title.trim_start().to_ascii_lowercase();
    l.starts_with("wip:") || l.starts_with("[wip]")
}

impl Gitea {
    pub fn new(conn: &Connection, creds: Arc<dyn CredentialSource>) -> Self {
        Self {
            id: conn.id.clone(),
            web: conn.base_url.trim_end_matches('/').to_string(),
            api: Api { client: crate::http::client_for(conn.internal), internal: conn.internal, base: conn.api_base(), style: AuthStyle::Token, creds, headers: vec![("Accept", "application/json")] },
        }
    }

    fn repo_path(r: &RepoRef) -> String {
        format!("repos/{}/{}", spec::encode_segment(&r.owner), spec::encode_segment(&r.name))
    }
}

fn summary(v: &Value) -> Option<RepoSummary> {
    Some(RepoSummary {
        repo: RepoRef::new(v["owner"]["login"].as_str().or(v["owner"]["username"].as_str())?, v["name"].as_str()?),
        description: s(v, "description"),
        private: v["private"].as_bool().unwrap_or(false),
        default_branch: s(v, "default_branch"),
        updated_at: s(v, "updated_at"),
        web_url: s(v, "html_url"),
        fork: v["fork"].as_bool().unwrap_or(false),
    })
}

fn meta(v: &Value) -> Option<RepoMeta> {
    let parent = v["parent"].as_object().and_then(|p| {
        let owner = p.get("owner")?;
        Some((
            RepoRef::new(owner.get("login").or(owner.get("username"))?.as_str()?, p.get("name")?.as_str()?),
            p.get("default_branch").and_then(Value::as_str).unwrap_or("").to_string(),
        ))
    });
    Some(RepoMeta {
        repo: RepoRef::new(v["owner"]["login"].as_str().or(v["owner"]["username"].as_str())?, v["name"].as_str()?),
        default_branch: s(v, "default_branch"),
        parent: parent.filter(|_| v["fork"].as_bool().unwrap_or(false)),
        private: v["private"].as_bool().unwrap_or(false),
        web_url: s(v, "html_url"),
        can_push: v["permissions"]["push"].as_bool(),
    })
}

fn pr(v: &Value) -> ChangeRequest {
    let state = if v["merged"].as_bool().unwrap_or(false) {
        CrState::Merged
    } else if v["state"] == "closed" {
        CrState::Closed
    } else {
        CrState::Open
    };
    let title = s(v, "title");
    ChangeRequest {
        number: id_str(&v["number"]),
        draft: v["draft"].as_bool().unwrap_or(false) || is_wip(&title),
        title,
        state,
        author: s(&v["user"], "login"),
        head: v["head"]["label"].as_str().or(v["head"]["ref"].as_str()).unwrap_or("").to_string(),
        base: s(&v["base"], "ref"),
        url: s(v, "html_url"),
        updated_at: s(v, "updated_at"),
        body: cap_text(v["body"].as_str().unwrap_or(""), MAX_TEXT_CHARS).0,
        head_sha: v["head"]["sha"].as_str().map(str::to_string),
    }
}

fn issue(v: &Value) -> Issue {
    Issue {
        number: id_str(&v["number"]),
        title: s(v, "title"),
        state: s(v, "state"),
        author: s(&v["user"], "login"),
        url: s(v, "html_url"),
        labels: v["labels"].as_array().map(|a| a.iter().filter_map(|l| l["name"].as_str().map(str::to_string)).collect()).unwrap_or_default(),
        body: cap_text(v["body"].as_str().unwrap_or(""), MAX_TEXT_CHARS).0,
        updated_at: s(v, "updated_at"),
    }
}

fn status_state(st: &str) -> CiState {
    match st {
        "success" => CiState::Success,
        "failure" | "error" => CiState::Failure,
        "pending" => CiState::Pending,
        "warning" => CiState::Success,
        _ => CiState::Unknown,
    }
}

fn state_param(f: StateFilter) -> String {
    match f {
        StateFilter::Open => "open",
        StateFilter::Closed => "closed",
        StateFilter::All => "all",
    }
    .into()
}

#[async_trait]
impl ForgeReader for Gitea {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Gitea
    }
    fn id(&self) -> &ForgeId {
        &self.id
    }

    async fn list_change_requests(&self, r: &RepoRef, state: StateFilter, limit: u32) -> Result<Vec<ChangeRequest>> {
        let q = [("state", state_param(state)), ("limit", limit.clamp(1, 50).to_string()), ("sort", "recentupdate".into())];
        let v = self.api.get(&format!("{}/pulls", Self::repo_path(r)), &q).await?;
        Ok(v.as_array().map(|a| a.iter().map(pr).collect()).unwrap_or_default())
    }

    async fn change_request(&self, r: &RepoRef, number: &str) -> Result<ChangeRequest> {
        Ok(pr(&self.api.get(&format!("{}/pulls/{}", Self::repo_path(r), spec::encode_segment(number)), &[]).await?))
    }

    async fn review_comments(&self, r: &RepoRef, number: &str) -> Result<Vec<ReviewComment>> {
        let n = spec::encode_segment(number);
        let body = |v: &Value| cap_text(v["body"].as_str().unwrap_or(""), 8000).0;
        let mut out = Vec::new();
        let reviews = self.api.get(&format!("{}/pulls/{n}/reviews", Self::repo_path(r)), &[("limit", "50".into())]).await?;
        for rv in reviews.as_array().into_iter().flatten().take(20) {
            if rv["body"].as_str().is_some_and(|b| !b.trim().is_empty()) {
                out.push(ReviewComment { author: s(&rv["user"], "login"), body: body(rv), path: None, line: None, created_at: s(rv, "submitted_at"), resolved: None });
            }
            if rv["comments_count"].as_u64().unwrap_or(1) == 0 {
                continue;
            }
            let rid = id_str(&rv["id"]);
            for c in self.api.get(&format!("{}/pulls/{n}/reviews/{rid}/comments", Self::repo_path(r)), &[]).await?.as_array().into_iter().flatten() {
                out.push(ReviewComment {
                    author: s(&c["user"], "login"),
                    body: body(c),
                    path: c["path"].as_str().map(str::to_string),
                    line: c["position"].as_u64().or(c["original_position"].as_u64()).filter(|l| *l > 0).map(|l| l as u32),
                    created_at: s(c, "created_at"),
                    resolved: c["resolver"].as_object().map(|_| true),
                });
            }
        }
        for c in self.api.get(&format!("{}/issues/{n}/comments", Self::repo_path(r)), &[("limit", "50".into())]).await?.as_array().into_iter().flatten() {
            out.push(ReviewComment { author: s(&c["user"], "login"), body: body(c), path: None, line: None, created_at: s(c, "created_at"), resolved: None });
        }
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(out)
    }

    async fn issues(&self, r: &RepoRef, state: StateFilter, limit: u32) -> Result<Vec<Issue>> {
        let q = [("type", "issues".to_string()), ("state", state_param(state)), ("limit", limit.clamp(1, 50).to_string())];
        let v = self.api.get(&format!("{}/issues", Self::repo_path(r)), &q).await?;
        Ok(v.as_array().map(|a| a.iter().map(issue).collect()).unwrap_or_default())
    }

    async fn issue(&self, r: &RepoRef, number: &str) -> Result<Issue> {
        Ok(issue(&self.api.get(&format!("{}/issues/{}", Self::repo_path(r), spec::encode_segment(number)), &[]).await?))
    }

    async fn ci_status(&self, r: &RepoRef, git_ref: &str) -> Result<CiStatus> {
        // Forgejo/Gitea Actions report through commit statuses too.
        let v = self.api.get(&format!("{}/commits/{}/status", Self::repo_path(r), spec::encode_path(git_ref)), &[]).await?;
        let checks = v["statuses"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| CiCheck {
                name: s(x, "context"),
                state: status_state(x["status"].as_str().or(x["state"].as_str()).unwrap_or("")),
                url: x["target_url"].as_str().filter(|u| !u.is_empty()).map(str::to_string),
            })
            .collect();
        Ok(CiStatus::from_checks(checks, None))
    }

    async fn file_at(&self, r: &RepoRef, path: &str, git_ref: &str) -> Result<FileAtRef> {
        let text = self.api.get_text(&format!("{}/raw/{}", Self::repo_path(r), spec::encode_path(path)), &[("ref", git_ref.to_string())], None).await?;
        crate::github::file_result(path, git_ref, &text)
    }
}

#[async_trait]
impl Forge for Gitea {
    fn web_base(&self) -> &str {
        &self.web
    }
    fn capabilities(&self) -> ForgeCaps {
        ForgeCaps { draft_cr: false, fork_api: true, ci_status: true, review_threads: true, issues: true, api_open_cr: true, device_flow: false }
    }
    fn creds(&self) -> &Arc<dyn CredentialSource> {
        &self.api.creds
    }

    async fn whoami(&self) -> Result<Option<ForgeUser>> {
        if self.api.creds.token(Purpose::Read).await?.is_none() {
            return Ok(None);
        }
        let v = self.api.get("user", &[]).await?;
        Ok(Some(ForgeUser { id: id_str(&v["id"]), login: v["login"].as_str().or(v["username"].as_str()).unwrap_or("").into(), name: s(&v, "full_name") }))
    }

    async fn list_repos(&self, q: &RepoQuery, login: &str) -> Result<Vec<RepoSummary>> {
        let page = vec![("limit", q.per_page.clamp(1, 50).to_string()), ("page", q.page.max(1).to_string())];
        let items: Vec<Value> = if let Some(text) = q.q.as_deref() {
            let mut p = page.clone();
            p.push(("q", text.to_string()));
            p.push(("sort", "updated".into()));
            p.push(("order", "desc".into()));
            let v = self.api.get("repos/search", &p).await?;
            let all = v["data"].as_array().cloned().unwrap_or_default();
            match q.owner.as_deref() {
                Some(o) => all.into_iter().filter(|r| r["owner"]["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(o))).collect(),
                None => all,
            }
        } else {
            let v = match q.owner.as_deref() {
                None => self.api.get("user/repos", &page).await?,
                Some(o) if o.eq_ignore_ascii_case(login) => {
                    let v = self.api.get("user/repos", &page).await?;
                    Value::Array(v.as_array().into_iter().flatten().filter(|r| r["owner"]["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(o))).cloned().collect())
                }
                Some(o) => match self.api.get(&format!("orgs/{}/repos", spec::encode_segment(o)), &page).await {
                    Ok(v) => v,
                    Err(_) => self.api.get(&format!("users/{}/repos", spec::encode_segment(o)), &page).await?,
                },
            };
            v.as_array().cloned().unwrap_or_default()
        };
        Ok(items.iter().filter_map(summary).collect())
    }

    async fn owners(&self, login: &str) -> Result<Vec<String>> {
        let orgs = self.api.get("user/orgs", &[("limit", "50".into())]).await?;
        let mut out = vec![login.to_string()];
        out.extend(orgs.as_array().into_iter().flatten().filter_map(|o| o["username"].as_str().or(o["name"].as_str()).map(str::to_string)));
        Ok(out)
    }

    async fn repo(&self, r: &RepoRef) -> Result<Option<RepoMeta>> {
        Ok(self.api.get_opt(&Self::repo_path(r), &[]).await?.as_ref().and_then(meta))
    }

    async fn create_fork(&self, upstream: &RepoRef) -> Result<RepoRef> {
        let (status, v) = self.api.write(Method::POST, &format!("{}/forks", Self::repo_path(upstream)), &json!({})).await?;
        if !status.is_success() {
            return Err(ForgeError::Rejected(format!("the forge didn't create the fork: {}", v.as_str().unwrap_or(""))));
        }
        meta(&v).map(|m| m.repo).ok_or_else(|| ForgeError::Transport("the fork answer had no repository".into()))
    }

    async fn open_change_request(&self, d: &ChangeRequestDraft) -> Result<ChangeRequestOutcome> {
        let cross = !d.head.same(&d.base);
        let head = if cross { format!("{}:{}", d.head.owner, d.head_branch) } else { d.head_branch.clone() };
        let title = if d.draft { wip_title(&d.title) } else { d.title.clone() };
        let path = format!("{}/pulls", Self::repo_path(&d.base));
        let (status, v) = self.api.write(Method::POST, &path, &json!({ "head": head, "base": d.base_branch, "title": title, "body": d.body })).await?;
        if status.is_success() {
            return Ok(ChangeRequestOutcome::Opened { number: id_str(&v["number"]), url: s(&v, "html_url"), draft_supported: false, existing: false });
        }
        let msg = v.as_str().unwrap_or("").to_string();
        if status == StatusCode::CONFLICT || msg.contains("already exists") {
            let list = self.api.get(&path, &[("state", "open".into()), ("limit", "50".into())]).await?;
            let found = list.as_array().into_iter().flatten().find(|p| {
                p["head"]["ref"] == d.head_branch.as_str()
                    && p["head"]["repo"]["full_name"].as_str().is_none_or(|f| f.eq_ignore_ascii_case(&d.head.full_name()))
            });
            if let Some(p) = found {
                return Ok(ChangeRequestOutcome::Opened { number: id_str(&p["number"]), url: s(p, "html_url"), draft_supported: false, existing: true });
            }
        }
        Err(ForgeError::Rejected(format!("the forge didn't open the pull request: {msg}")))
    }

    fn git_auth_args(&self) -> Vec<String> {
        match self.api.creds.git_helper() {
            Some(h) => tokens::helper_args(&self.web, &h),
            None => Vec::new(),
        }
    }
}

impl Gitea {
    /// uid → current login (anonymous `users/search?uid=`).
    pub async fn resolve_user(&self, subject_id: &str) -> Result<ForgeUser> {
        let v = self.api.get("users/search", &[("uid", subject_id.to_string())]).await?;
        let u = v["data"].as_array().and_then(|a| a.first()).ok_or_else(|| ForgeError::NotFound(format!("user {subject_id}")))?;
        Ok(ForgeUser { id: id_str(&u["id"]), login: s(u, "login"), name: s(u, "full_name") })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wip_titles_are_prefixed_once() {
        assert_eq!(super::wip_title("Fix"), "WIP: Fix");
        assert_eq!(super::wip_title("[WIP] Fix"), "[WIP] Fix");
        assert!(super::is_wip("wip: x") && !super::is_wip("Fix wip:"));
    }
}

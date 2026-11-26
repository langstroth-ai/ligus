//! GitHub (github.com, and GitHub Enterprise Server through `base_url`).
//!
//! REST v3 pinned to API version 2022-11-28
//! (<https://docs.github.com/en/rest/about-the-rest-api/api-versions>).
//! Draft PRs use the real `draft` flag. CI reads the combined commit status
//! and, where the credential allows it, check runs: fine-grained personal
//! access tokens can't read the Checks API
//! (<https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens>),
//! so a 403 there degrades to statuses only, with a note.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

use crate::http::{id_str, s, Api, AuthStyle};
use crate::tokens::CredentialSource;
use crate::*;

pub struct GitHub {
    id: ForgeId,
    web: String,
    api: Api,
    login_hint: Option<String>,
}

impl GitHub {
    pub fn new(conn: &Connection, creds: Arc<dyn CredentialSource>) -> Self {
        Self {
            id: conn.id.clone(),
            web: conn.base_url.trim_end_matches('/').to_string(),
            api: Api {
                client: crate::http::client_for(conn.internal),
                internal: conn.internal,
                base: conn.api_base(),
                style: AuthStyle::Bearer,
                creds,
                headers: vec![("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")],
            },
            login_hint: conn.login.clone(),
        }
    }

    fn repo_path(r: &RepoRef) -> String {
        format!("repos/{}/{}", spec::encode_segment(&r.owner), spec::encode_segment(&r.name))
    }
}

fn summary(v: &Value) -> Option<RepoSummary> {
    Some(RepoSummary {
        repo: RepoRef::new(v["owner"]["login"].as_str()?, v["name"].as_str()?),
        description: s(v, "description"),
        private: v["private"].as_bool().unwrap_or(false),
        default_branch: s(v, "default_branch"),
        updated_at: v["pushed_at"].as_str().or(v["updated_at"].as_str()).unwrap_or("").to_string(),
        web_url: s(v, "html_url"),
        fork: v["fork"].as_bool().unwrap_or(false),
    })
}

pub(crate) fn meta(v: &Value) -> Option<RepoMeta> {
    let parent = v["parent"].as_object().and_then(|p| {
        Some((RepoRef::new(p.get("owner")?.get("login")?.as_str()?, p.get("name")?.as_str()?), p.get("default_branch").and_then(Value::as_str).unwrap_or("").to_string()))
    });
    Some(RepoMeta {
        repo: RepoRef::new(v["owner"]["login"].as_str()?, v["name"].as_str()?),
        default_branch: s(v, "default_branch"),
        parent: parent.filter(|_| v["fork"].as_bool().unwrap_or(false)),
        private: v["private"].as_bool().unwrap_or(false),
        web_url: s(v, "html_url"),
        can_push: v["permissions"]["push"].as_bool(),
    })
}

fn pr(v: &Value) -> ChangeRequest {
    let state = if !v["merged_at"].is_null() && v["merged_at"].is_string() {
        CrState::Merged
    } else if v["state"] == "closed" {
        CrState::Closed
    } else {
        CrState::Open
    };
    ChangeRequest {
        number: id_str(&v["number"]),
        title: s(v, "title"),
        state,
        draft: v["draft"].as_bool().unwrap_or(false),
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
        labels: v["labels"].as_array().map(|a| a.iter().filter_map(|l| l["name"].as_str().or(l.as_str()).map(str::to_string)).collect()).unwrap_or_default(),
        body: cap_text(v["body"].as_str().unwrap_or(""), MAX_TEXT_CHARS).0,
        updated_at: s(v, "updated_at"),
    }
}

fn status_state(st: &str) -> CiState {
    match st {
        "success" => CiState::Success,
        "failure" | "error" => CiState::Failure,
        "pending" => CiState::Pending,
        _ => CiState::Unknown,
    }
}

fn check_state(v: &Value) -> CiState {
    match (v["status"].as_str().unwrap_or(""), v["conclusion"].as_str().unwrap_or("")) {
        ("completed", "success" | "neutral" | "skipped") => CiState::Success,
        ("completed", "cancelled") => CiState::Cancelled,
        ("completed", _) => CiState::Failure,
        ("in_progress", _) => CiState::Running,
        ("queued" | "waiting" | "requested" | "pending", _) => CiState::Pending,
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
impl ForgeReader for GitHub {
    fn kind(&self) -> ForgeKind {
        ForgeKind::GitHub
    }
    fn id(&self) -> &ForgeId {
        &self.id
    }

    async fn list_change_requests(&self, r: &RepoRef, state: StateFilter, limit: u32) -> Result<Vec<ChangeRequest>> {
        let q = [("state", state_param(state)), ("per_page", limit.clamp(1, 100).to_string()), ("sort", "updated".into()), ("direction", "desc".into())];
        let v = self.api.get(&format!("{}/pulls", Self::repo_path(r)), &q).await?;
        Ok(v.as_array().map(|a| a.iter().map(pr).collect()).unwrap_or_default())
    }

    async fn change_request(&self, r: &RepoRef, number: &str) -> Result<ChangeRequest> {
        Ok(pr(&self.api.get(&format!("{}/pulls/{}", Self::repo_path(r), spec::encode_segment(number)), &[]).await?))
    }

    async fn review_comments(&self, r: &RepoRef, number: &str) -> Result<Vec<ReviewComment>> {
        let n = spec::encode_segment(number);
        let page = [("per_page", "100".to_string())];
        let mut out = Vec::new();
        let body = |v: &Value| cap_text(v["body"].as_str().unwrap_or(""), 8000).0;
        // Review summaries (approve / request changes with a body).
        for v in self.api.get(&format!("{}/pulls/{n}/reviews", Self::repo_path(r)), &page).await?.as_array().into_iter().flatten() {
            if v["body"].as_str().is_some_and(|b| !b.trim().is_empty()) {
                out.push(ReviewComment { author: s(&v["user"], "login"), body: body(v), path: None, line: None, created_at: s(v, "submitted_at"), resolved: None });
            }
        }
        // Inline comments on the diff.
        for v in self.api.get(&format!("{}/pulls/{n}/comments", Self::repo_path(r)), &page).await?.as_array().into_iter().flatten() {
            out.push(ReviewComment {
                author: s(&v["user"], "login"),
                body: body(v),
                path: v["path"].as_str().map(str::to_string),
                line: v["line"].as_u64().or(v["original_line"].as_u64()).map(|l| l as u32),
                created_at: s(v, "created_at"),
                resolved: None,
            });
        }
        // The conversation.
        for v in self.api.get(&format!("{}/issues/{n}/comments", Self::repo_path(r)), &page).await?.as_array().into_iter().flatten() {
            out.push(ReviewComment { author: s(&v["user"], "login"), body: body(v), path: None, line: None, created_at: s(v, "created_at"), resolved: None });
        }
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(out)
    }

    async fn issues(&self, r: &RepoRef, state: StateFilter, limit: u32) -> Result<Vec<Issue>> {
        let q = [("state", state_param(state)), ("per_page", limit.clamp(1, 100).to_string()), ("sort", "updated".into())];
        let v = self.api.get(&format!("{}/issues", Self::repo_path(r)), &q).await?;
        // The issues endpoint lists pull requests too.
        Ok(v.as_array().map(|a| a.iter().filter(|i| i.get("pull_request").is_none()).map(issue).collect()).unwrap_or_default())
    }

    async fn issue(&self, r: &RepoRef, number: &str) -> Result<Issue> {
        Ok(issue(&self.api.get(&format!("{}/issues/{}", Self::repo_path(r), spec::encode_segment(number)), &[]).await?))
    }

    async fn ci_status(&self, r: &RepoRef, git_ref: &str) -> Result<CiStatus> {
        let gr = spec::encode_path(git_ref);
        let mut checks = Vec::new();
        let combined = self.api.get(&format!("{}/commits/{gr}/status", Self::repo_path(r)), &[("per_page", "100".into())]).await?;
        for st in combined["statuses"].as_array().into_iter().flatten() {
            checks.push(CiCheck { name: s(st, "context"), state: status_state(st["state"].as_str().unwrap_or("")), url: st["target_url"].as_str().map(str::to_string) });
        }
        let note = match self.api.get(&format!("{}/commits/{gr}/check-runs", Self::repo_path(r)), &[("per_page", "100".into())]).await {
            Ok(v) => {
                for c in v["check_runs"].as_array().into_iter().flatten() {
                    checks.push(CiCheck { name: s(c, "name"), state: check_state(c), url: c["html_url"].as_str().map(str::to_string) });
                }
                None
            }
            Err(ForgeError::Unauthorized(_)) => Some("checks unavailable with this token (fine-grained tokens can't read the Checks API); commit statuses only".into()),
            Err(e) => return Err(e),
        };
        Ok(CiStatus::from_checks(checks, note))
    }

    async fn file_at(&self, r: &RepoRef, path: &str, git_ref: &str) -> Result<FileAtRef> {
        let text = self
            .api
            .get_text(&format!("{}/contents/{}", Self::repo_path(r), spec::encode_path(path)), &[("ref", git_ref.to_string())], Some("application/vnd.github.raw+json"))
            .await?;
        file_result(path, git_ref, &text)
    }
}

pub(crate) fn file_result(path: &str, git_ref: &str, text: &str) -> Result<FileAtRef> {
    if text.contains('\0') {
        return Err(ForgeError::Unsupported(format!("{path} is a binary file")));
    }
    let (text, truncated) = cap_text(text, MAX_TEXT_CHARS);
    Ok(FileAtRef { path: path.to_string(), git_ref: git_ref.to_string(), text, truncated })
}

#[async_trait]
impl Forge for GitHub {
    fn web_base(&self) -> &str {
        &self.web
    }
    fn capabilities(&self) -> ForgeCaps {
        ForgeCaps { draft_cr: true, fork_api: true, ci_status: true, review_threads: true, issues: true, api_open_cr: true, device_flow: true }
    }
    fn creds(&self) -> &Arc<dyn CredentialSource> {
        &self.api.creds
    }

    async fn whoami(&self) -> Result<Option<ForgeUser>> {
        if self.api.creds.token(Purpose::Read).await?.is_none() {
            return Ok(None);
        }
        let v = self.api.get("user", &[]).await?;
        Ok(Some(ForgeUser { id: id_str(&v["id"]), login: s(&v, "login"), name: s(&v, "name") }))
    }

    async fn list_repos(&self, q: &RepoQuery, login: &str) -> Result<Vec<RepoSummary>> {
        let page = vec![("per_page", q.per_page.clamp(1, 100).to_string()), ("page", q.page.max(1).to_string())];
        let items: Vec<Value> = if let Some(text) = q.q.as_deref() {
            let scope = q.owner.as_deref().unwrap_or(login);
            let mut p = page.clone();
            p.push(("q", format!("{text} user:{scope} fork:true")));
            p.push(("sort", "updated".into()));
            let v = self.api.get("search/repositories", &p).await?;
            v["items"].as_array().cloned().unwrap_or_default()
        } else {
            let mut p = page.clone();
            p.push(("sort", "updated".into()));
            let v = match q.owner.as_deref() {
                None => {
                    p.push(("affiliation", "owner,collaborator,organization_member".into()));
                    self.api.get("user/repos", &p).await?
                }
                Some(o) if o.eq_ignore_ascii_case(login) => {
                    p.push(("affiliation", "owner".into()));
                    self.api.get("user/repos", &p).await?
                }
                Some(o) => match self.api.get(&format!("orgs/{}/repos", spec::encode_segment(o)), &p).await {
                    Ok(v) => v,
                    Err(_) => self.api.get(&format!("users/{}/repos", spec::encode_segment(o)), &p).await?,
                },
            };
            v.as_array().cloned().unwrap_or_default()
        };
        Ok(items.iter().filter_map(summary).collect())
    }

    async fn owners(&self, login: &str) -> Result<Vec<String>> {
        let orgs = self.api.get("user/orgs", &[("per_page", "100".into())]).await?;
        let mut out = vec![login.to_string()];
        out.extend(orgs.as_array().into_iter().flatten().filter_map(|o| o["login"].as_str().map(str::to_string)));
        Ok(out)
    }

    async fn repo(&self, r: &RepoRef) -> Result<Option<RepoMeta>> {
        Ok(self.api.get_opt(&Self::repo_path(r), &[]).await?.as_ref().and_then(meta))
    }

    async fn create_fork(&self, upstream: &RepoRef) -> Result<RepoRef> {
        let (status, v) = self.api.write(Method::POST, &format!("{}/forks", Self::repo_path(upstream)), &json!({})).await?;
        if !status.is_success() {
            return Err(ForgeError::Rejected(format!("GitHub didn't create the fork: {}", v.as_str().unwrap_or(""))));
        }
        meta(&v).map(|m| m.repo).ok_or_else(|| ForgeError::Transport("GitHub's fork answer had no repository".into()))
    }

    async fn create_private_repo(&self, owner: &str, login: &str, name: &str) -> Result<RepoRef> {
        if !spec::valid_segment(name) || name.ends_with(".git") || (!owner.is_empty() && !spec::valid_segment(owner)) {
            return Err(ForgeError::Invalid("not a repository name".into()));
        }
        // Always private, never initialised (the scratch repo's history is
        // pushed into it as it is).
        let body = json!({ "name": name, "private": true, "auto_init": false, "has_wiki": false });
        let path = if owner.is_empty() || owner.eq_ignore_ascii_case(login) { "user/repos".to_string() } else { format!("orgs/{}/repos", spec::encode_segment(owner)) };
        let (status, v) = self.api.write(Method::POST, &path, &body).await?;
        if !status.is_success() {
            return Err(ForgeError::Rejected(format!("GitHub didn't create the repository: {}", v.as_str().unwrap_or(""))));
        }
        let m = meta(&v).ok_or_else(|| ForgeError::Transport("GitHub's answer had no repository".into()))?;
        // GitHub created it private; refuse to go on if it says otherwise
        // (an Enterprise "internal" repository also says private: true).
        let visibility = v["visibility"].as_str();
        if v["private"].as_bool() != Some(true) || visibility.is_some_and(|x| x != "private") {
            return Err(ForgeError::Rejected(format!(
                "GitHub created {} but didn't report it as private; nothing was pushed (delete it on GitHub if you don't want it)",
                m.repo.full_name()
            )));
        }
        Ok(m.repo)
    }

    async fn open_change_request(&self, d: &ChangeRequestDraft) -> Result<ChangeRequestOutcome> {
        let cross = !d.head.same(&d.base);
        let head = if cross { format!("{}:{}", d.head.owner, d.head_branch) } else { d.head_branch.clone() };
        let mut body = json!({ "title": d.title, "body": d.body, "head": head, "base": d.base_branch, "draft": d.draft });
        // A fork in the same org as its parent needs head_repo to say which.
        if cross && d.head.owner.eq_ignore_ascii_case(&d.base.owner) {
            body["head_repo"] = json!(d.head.name);
            body["head"] = json!(d.head_branch);
        }
        let path = format!("{}/pulls", Self::repo_path(&d.base));
        let (mut status, mut v) = self.api.write(Method::POST, &path, &body).await?;
        let mut draft = d.draft;
        // Some plans have no draft PRs: open it as a normal PR then.
        if status == StatusCode::UNPROCESSABLE_ENTITY && d.draft && v.as_str().is_some_and(|m| m.to_ascii_lowercase().contains("draft")) {
            body["draft"] = json!(false);
            draft = false;
            (status, v) = self.api.write(Method::POST, &path, &body).await?;
        }
        if status.is_success() {
            return Ok(ChangeRequestOutcome::Opened { number: id_str(&v["number"]), url: s(&v, "html_url"), draft_supported: draft || !d.draft, existing: false });
        }
        let msg = v.as_str().unwrap_or("").to_string();
        if status == StatusCode::UNPROCESSABLE_ENTITY && msg.contains("already exists") {
            let q = [("head", format!("{}:{}", d.head.owner, d.head_branch)), ("state", "open".to_string())];
            let list = self.api.get(&path, &q).await?;
            if let Some(p) = list.as_array().and_then(|a| a.first()) {
                return Ok(ChangeRequestOutcome::Opened { number: id_str(&p["number"]), url: s(p, "html_url"), draft_supported: true, existing: true });
            }
        }
        Err(ForgeError::Rejected(format!("GitHub didn't open the pull request: {msg}")))
    }

    fn git_auth_args(&self) -> Vec<String> {
        match self.api.creds.git_helper() {
            Some(h) => tokens::helper_args(&self.web, &h),
            None => Vec::new(),
        }
    }
}

impl GitHub {
    /// The login hint saved on the connection (for the git username).
    pub fn login_hint(&self) -> Option<&str> {
        self.login_hint.as_deref()
    }

    /// Anonymous identity-link reads: id → current login.
    pub async fn resolve_user(&self, subject_id: &str) -> Result<ForgeUser> {
        let v = self.api.get(&format!("user/{}", spec::encode_segment(subject_id)), &[]).await?;
        Ok(ForgeUser { id: id_str(&v["id"]), login: s(&v, "login"), name: s(&v, "name") })
    }

    /// The account's public SSH keys (authentication and signing).
    pub async fn public_keys(&self, subject_id: &str) -> Result<Vec<PublicKeyEntry>> {
        let u = self.resolve_user(subject_id).await?;
        let login = spec::encode_segment(&u.login);
        let mut out = Vec::new();
        for (path, usage) in [(format!("users/{login}/keys"), KeyUsage::Auth), (format!("users/{login}/ssh_signing_keys"), KeyUsage::Signing)] {
            for k in self.api.get(&path, &[("per_page", "100".into())]).await?.as_array().into_iter().flatten() {
                if let Some(key) = k["key"].as_str() {
                    out.push(PublicKeyEntry { openssh: key.to_string(), usage });
                }
            }
        }
        Ok(out)
    }
}

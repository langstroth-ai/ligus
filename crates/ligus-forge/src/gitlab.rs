//! GitLab (gitlab.com and self-managed through `base_url`), REST v4.
//!
//! - Projects are addressed by their URL-encoded path (`group%2Fsub%2Fname`);
//!   owners may be nested groups.
//! - Merge requests have no `draft` field on create, so a draft MR is a
//!   `Draft:` title prefix (<https://docs.gitlab.com/api/merge_requests/>).
//!   A cross-fork MR is created on the source (fork) project with
//!   `target_project_id`. An existing MR for the branch answers 409.
//! - Tokens go as `Authorization: Bearer`, which GitLab accepts for OAuth
//!   tokens and personal/project/group access tokens alike
//!   (<https://docs.gitlab.com/api/rest/authentication/>).

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

use crate::http::{id_str, s, Api, AuthStyle};
use crate::tokens::CredentialSource;
use crate::*;

pub struct GitLab {
    id: ForgeId,
    web: String,
    api: Api,
}

/// The title of a draft merge request.
pub fn draft_title(title: &str) -> String {
    let t = title.trim();
    let lower = t.to_ascii_lowercase();
    if ["draft:", "[draft]", "(draft)"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        t.to_string()
    } else {
        format!("Draft: {t}")
    }
}

impl GitLab {
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
                headers: vec![("Accept", "application/json")],
            },
        }
    }

    fn project(r: &RepoRef) -> String {
        format!("projects/{}", spec::encode_segment(&r.full_name()))
    }
}

fn repo_of(path_with_namespace: &str) -> Option<RepoRef> {
    let (o, n) = path_with_namespace.rsplit_once('/')?;
    Some(RepoRef::new(o, n))
}

fn summary(v: &Value) -> Option<RepoSummary> {
    Some(RepoSummary {
        repo: repo_of(v["path_with_namespace"].as_str()?)?,
        description: s(v, "description"),
        private: v["visibility"].as_str().is_some_and(|x| x != "public"),
        default_branch: s(v, "default_branch"),
        updated_at: s(v, "last_activity_at"),
        web_url: s(v, "web_url"),
        fork: v["forked_from_project"].is_object(),
    })
}

fn access_level(v: &Value) -> Option<u64> {
    let p = &v["permissions"];
    let a = p["project_access"]["access_level"].as_u64();
    let g = p["group_access"]["access_level"].as_u64();
    match (a, g) {
        (None, None) => None,
        (a, g) => Some(a.unwrap_or(0).max(g.unwrap_or(0))),
    }
}

fn meta(v: &Value) -> Option<RepoMeta> {
    let parent = v["forked_from_project"].as_object().and_then(|p| {
        Some((
            repo_of(p.get("path_with_namespace")?.as_str()?)?,
            p.get("default_branch")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ))
    });
    Some(RepoMeta {
        repo: repo_of(v["path_with_namespace"].as_str()?)?,
        default_branch: s(v, "default_branch"),
        parent,
        private: v["visibility"].as_str().is_some_and(|x| x != "public"),
        web_url: s(v, "web_url"),
        // Developer (30) and above can push.
        can_push: access_level(v).map(|l| l >= 30),
    })
}

fn mr(v: &Value) -> ChangeRequest {
    let state = match v["state"].as_str().unwrap_or("") {
        "merged" => CrState::Merged,
        "closed" | "locked" => CrState::Closed,
        _ => CrState::Open,
    };
    ChangeRequest {
        number: id_str(&v["iid"]),
        title: s(v, "title"),
        state,
        draft: v["draft"]
            .as_bool()
            .or(v["work_in_progress"].as_bool())
            .unwrap_or(false),
        author: s(&v["author"], "username"),
        head: s(v, "source_branch"),
        base: s(v, "target_branch"),
        url: s(v, "web_url"),
        updated_at: s(v, "updated_at"),
        body: cap_text(v["description"].as_str().unwrap_or(""), MAX_TEXT_CHARS).0,
        head_sha: v["sha"].as_str().map(str::to_string),
    }
}

fn issue(v: &Value) -> Issue {
    Issue {
        number: id_str(&v["iid"]),
        title: s(v, "title"),
        state: s(v, "state"),
        author: s(&v["author"], "username"),
        url: s(v, "web_url"),
        labels: v["labels"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.as_str().or(l["name"].as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        body: cap_text(v["description"].as_str().unwrap_or(""), MAX_TEXT_CHARS).0,
        updated_at: s(v, "updated_at"),
    }
}

fn ci_state(st: &str) -> CiState {
    match st {
        "success" => CiState::Success,
        "failed" => CiState::Failure,
        "canceled" | "cancelled" => CiState::Cancelled,
        "running" => CiState::Running,
        "pending" | "created" | "waiting_for_resource" | "preparing" | "scheduled" | "manual" => {
            CiState::Pending
        }
        "skipped" => CiState::Success,
        _ => CiState::Unknown,
    }
}

fn state_param(f: StateFilter) -> String {
    match f {
        StateFilter::Open => "opened",
        StateFilter::Closed => "closed",
        StateFilter::All => "all",
    }
    .into()
}

#[async_trait]
impl ForgeReader for GitLab {
    fn kind(&self) -> ForgeKind {
        ForgeKind::GitLab
    }
    fn id(&self) -> &ForgeId {
        &self.id
    }

    async fn list_change_requests(
        &self,
        r: &RepoRef,
        state: StateFilter,
        limit: u32,
    ) -> Result<Vec<ChangeRequest>> {
        let q = [
            ("state", state_param(state)),
            ("per_page", limit.clamp(1, 100).to_string()),
            ("order_by", "updated_at".into()),
        ];
        let v = self
            .api
            .get(&format!("{}/merge_requests", Self::project(r)), &q)
            .await?;
        Ok(v.as_array()
            .map(|a| a.iter().map(mr).collect())
            .unwrap_or_default())
    }

    async fn change_request(&self, r: &RepoRef, number: &str) -> Result<ChangeRequest> {
        Ok(mr(&self
            .api
            .get(
                &format!(
                    "{}/merge_requests/{}",
                    Self::project(r),
                    spec::encode_segment(number)
                ),
                &[],
            )
            .await?))
    }

    async fn review_comments(&self, r: &RepoRef, number: &str) -> Result<Vec<ReviewComment>> {
        let v = self
            .api
            .get(
                &format!(
                    "{}/merge_requests/{}/discussions",
                    Self::project(r),
                    spec::encode_segment(number)
                ),
                &[("per_page", "100".into())],
            )
            .await?;
        let mut out = Vec::new();
        for d in v.as_array().into_iter().flatten() {
            for n in d["notes"].as_array().into_iter().flatten() {
                if n["system"].as_bool().unwrap_or(false) {
                    continue;
                }
                out.push(ReviewComment {
                    author: s(&n["author"], "username"),
                    body: cap_text(n["body"].as_str().unwrap_or(""), 8000).0,
                    path: n["position"]["new_path"].as_str().map(str::to_string),
                    line: n["position"]["new_line"]
                        .as_u64()
                        .or(n["position"]["old_line"].as_u64())
                        .map(|l| l as u32),
                    created_at: s(n, "created_at"),
                    resolved: n["resolvable"]
                        .as_bool()
                        .filter(|r| *r)
                        .and(n["resolved"].as_bool()),
                });
            }
        }
        Ok(out)
    }

    async fn issues(&self, r: &RepoRef, state: StateFilter, limit: u32) -> Result<Vec<Issue>> {
        let q = [
            ("state", state_param(state)),
            ("per_page", limit.clamp(1, 100).to_string()),
            ("order_by", "updated_at".into()),
        ];
        let v = self
            .api
            .get(&format!("{}/issues", Self::project(r)), &q)
            .await?;
        Ok(v.as_array()
            .map(|a| a.iter().map(issue).collect())
            .unwrap_or_default())
    }

    async fn issue(&self, r: &RepoRef, number: &str) -> Result<Issue> {
        Ok(issue(
            &self
                .api
                .get(
                    &format!(
                        "{}/issues/{}",
                        Self::project(r),
                        spec::encode_segment(number)
                    ),
                    &[],
                )
                .await?,
        ))
    }

    async fn ci_status(&self, r: &RepoRef, git_ref: &str) -> Result<CiStatus> {
        // The ref (branch, tag or sha) → its commit and last pipeline.
        let c = self
            .api
            .get(
                &format!(
                    "{}/repository/commits/{}",
                    Self::project(r),
                    spec::encode_segment(git_ref)
                ),
                &[],
            )
            .await?;
        let sha = c["id"].as_str().unwrap_or(git_ref).to_string();
        let mut checks = Vec::new();
        if let Some(p) = c["last_pipeline"].as_object() {
            checks.push(CiCheck {
                name: format!("pipeline #{}", p.get("id").map(id_str).unwrap_or_default()),
                state: ci_state(p.get("status").and_then(Value::as_str).unwrap_or("")),
                url: p.get("web_url").and_then(Value::as_str).map(str::to_string),
            });
        }
        let st = self
            .api
            .get(
                &format!(
                    "{}/repository/commits/{}/statuses",
                    Self::project(r),
                    spec::encode_segment(&sha)
                ),
                &[("per_page", "100".into())],
            )
            .await?;
        for x in st.as_array().into_iter().flatten() {
            checks.push(CiCheck {
                name: s(x, "name"),
                state: ci_state(x["status"].as_str().unwrap_or("")),
                url: x["target_url"].as_str().map(str::to_string),
            });
        }
        Ok(CiStatus::from_checks(checks, None))
    }

    async fn file_at(&self, r: &RepoRef, path: &str, git_ref: &str) -> Result<FileAtRef> {
        let text = self
            .api
            .get_text(
                &format!(
                    "{}/repository/files/{}/raw",
                    Self::project(r),
                    spec::encode_segment(path)
                ),
                &[("ref", git_ref.to_string())],
                None,
            )
            .await?;
        crate::github::file_result(path, git_ref, &text)
    }
}

#[async_trait]
impl Forge for GitLab {
    fn web_base(&self) -> &str {
        &self.web
    }
    fn capabilities(&self) -> ForgeCaps {
        ForgeCaps {
            draft_cr: false,
            fork_api: true,
            ci_status: true,
            review_threads: true,
            issues: true,
            api_open_cr: true,
            device_flow: true,
        }
    }
    fn creds(&self) -> &Arc<dyn CredentialSource> {
        &self.api.creds
    }

    async fn whoami(&self) -> Result<Option<ForgeUser>> {
        if self.api.creds.token(Purpose::Read).await?.is_none() {
            return Ok(None);
        }
        let v = self.api.get("user", &[]).await?;
        Ok(Some(ForgeUser {
            id: id_str(&v["id"]),
            login: s(&v, "username"),
            name: s(&v, "name"),
        }))
    }

    async fn list_repos(&self, q: &RepoQuery, login: &str) -> Result<Vec<RepoSummary>> {
        let mut p = vec![
            ("per_page", q.per_page.clamp(1, 100).to_string()),
            ("page", q.page.max(1).to_string()),
            ("order_by", "last_activity_at".into()),
        ];
        if let Some(t) = q.q.as_deref() {
            p.push(("search", t.to_string()));
        }
        let v = match q.owner.as_deref() {
            None => {
                p.push(("membership", "true".into()));
                self.api.get("projects", &p).await?
            }
            Some(o) if o.eq_ignore_ascii_case(login) => {
                self.api
                    .get(&format!("users/{}/projects", spec::encode_segment(o)), &p)
                    .await?
            }
            Some(o) => {
                p.push(("include_subgroups", "true".into()));
                self.api
                    .get(&format!("groups/{}/projects", spec::encode_segment(o)), &p)
                    .await?
            }
        };
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(summary)
            .collect())
    }

    async fn owners(&self, login: &str) -> Result<Vec<String>> {
        let groups = self
            .api
            .get(
                "groups",
                &[
                    ("per_page", "100".into()),
                    ("min_access_level", "30".into()),
                ],
            )
            .await?;
        let mut out = vec![login.to_string()];
        out.extend(
            groups
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|g| g["full_path"].as_str().map(str::to_string)),
        );
        Ok(out)
    }

    async fn repo(&self, r: &RepoRef) -> Result<Option<RepoMeta>> {
        Ok(self
            .api
            .get_opt(&Self::project(r), &[])
            .await?
            .as_ref()
            .and_then(meta))
    }

    async fn create_fork(&self, upstream: &RepoRef) -> Result<RepoRef> {
        let (status, v) = self
            .api
            .write(
                Method::POST,
                &format!("{}/fork", Self::project(upstream)),
                &json!({}),
            )
            .await?;
        if !status.is_success() {
            return Err(ForgeError::Rejected(format!(
                "GitLab didn't create the fork: {}",
                v.as_str().unwrap_or("")
            )));
        }
        v["path_with_namespace"]
            .as_str()
            .and_then(repo_of)
            .ok_or_else(|| ForgeError::Transport("GitLab's fork answer had no project".into()))
    }

    async fn open_change_request(&self, d: &ChangeRequestDraft) -> Result<ChangeRequestOutcome> {
        if !self.api.creds.has_write() {
            return Err(ForgeError::NotSignedIn(
                "this needs a write token for the code host on this device".into(),
            ));
        }
        let title = if d.draft {
            draft_title(&d.title)
        } else {
            d.title.clone()
        };
        let mut body = json!({ "source_branch": d.head_branch, "target_branch": d.base_branch, "title": title, "description": d.body, "remove_source_branch": false });
        let cross = !d.head.same(&d.base);
        let ids = if cross {
            let pid = |v: Value| {
                v["id"]
                    .as_u64()
                    .ok_or_else(|| ForgeError::Transport("GitLab didn't give a project id".into()))
            };
            let base = pid(self.api.get(&Self::project(&d.base), &[]).await?)?;
            let head = pid(self.api.get(&Self::project(&d.head), &[]).await?)?;
            body["target_project_id"] = json!(base);
            Some((base, head))
        } else {
            None
        };
        let (status, v) = self
            .api
            .write(
                Method::POST,
                &format!("{}/merge_requests", Self::project(&d.head)),
                &body,
            )
            .await?;
        if status.is_success() {
            return Ok(ChangeRequestOutcome::Opened {
                number: id_str(&v["iid"]),
                url: s(&v, "web_url"),
                draft_supported: false,
                existing: false,
            });
        }
        let msg = v.as_str().unwrap_or("").to_string();
        if status == StatusCode::CONFLICT && msg.contains("already exists") {
            let target = match ids {
                Some((base, _)) => format!("projects/{base}"),
                None => Self::project(&d.base),
            };
            let list = self
                .api
                .get(
                    &format!("{target}/merge_requests"),
                    &[
                        ("state", "opened".into()),
                        ("source_branch", d.head_branch.clone()),
                    ],
                )
                .await?;
            let found = list.as_array().into_iter().flatten().find(|m| {
                m["source_branch"] == d.head_branch.as_str()
                    && ids.is_none_or(|(_, head)| m["source_project_id"].as_u64() == Some(head))
            });
            if let Some(m) = found {
                return Ok(ChangeRequestOutcome::Opened {
                    number: id_str(&m["iid"]),
                    url: s(m, "web_url"),
                    draft_supported: false,
                    existing: true,
                });
            }
        }
        Err(ForgeError::Rejected(format!(
            "GitLab didn't open the merge request: {msg}"
        )))
    }

    fn git_auth_args(&self) -> Vec<String> {
        match self.api.creds.git_helper() {
            Some(h) => tokens::helper_args(&self.web, &h),
            None => Vec::new(),
        }
    }
}

impl GitLab {
    pub async fn resolve_user(&self, subject_id: &str) -> Result<ForgeUser> {
        let v = self
            .api
            .get(&format!("users/{}", spec::encode_segment(subject_id)), &[])
            .await?;
        Ok(ForgeUser {
            id: id_str(&v["id"]),
            login: s(&v, "username"),
            name: s(&v, "name"),
        })
    }

    /// Public keys by user id (anonymous on GitLab), with their usage.
    pub async fn public_keys(&self, subject_id: &str) -> Result<Vec<PublicKeyEntry>> {
        let v = self
            .api
            .get(
                &format!("users/{}/keys", spec::encode_segment(subject_id)),
                &[],
            )
            .await?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| {
                let usage = match k["usage_type"].as_str().unwrap_or("") {
                    "auth" => KeyUsage::Auth,
                    "signing" => KeyUsage::Signing,
                    "auth_and_signing" => KeyUsage::Both,
                    _ => KeyUsage::Unknown,
                };
                Some(PublicKeyEntry {
                    openssh: k["key"].as_str()?.to_string(),
                    usage,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn draft_titles_are_prefixed_once() {
        assert_eq!(super::draft_title("Fix it"), "Draft: Fix it");
        assert_eq!(super::draft_title("Draft: Fix it"), "Draft: Fix it");
        assert_eq!(super::draft_title("[Draft] x"), "[Draft] x");
    }
}

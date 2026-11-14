//! Contract tests per forge against a recorded fake HTTP server: no real
//! accounts, no network beyond loopback. Each test pins the requests the
//! implementation makes (method, path, query, auth header, body) and how it
//! reads the answers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use hive_forge::tokens::{AuthSource, MemoryTokens, Purpose, Secret, StoreCredentials, TokenStore};
use hive_forge::*;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    auth: Option<String>,
    accept: Option<String>,
    version: Option<String>,
    body: Value,
}

type Route = (Method, String, u16, Value, Option<String>);

#[derive(Clone, Default)]
struct Fake {
    routes: Arc<Mutex<Vec<Route>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fake {
    fn on(&self, method: Method, path: &str, status: u16, body: Value) -> &Self {
        self.routes.lock().unwrap().push((method, path.to_string(), status, body, None));
        self
    }
    fn redirect(&self, path: &str, to: &str) -> &Self {
        self.routes.lock().unwrap().push((Method::GET, path.to_string(), 302, Value::Null, Some(to.to_string())));
        self
    }
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    fn find(&self, method: &str, path: &str) -> Seen {
        self.seen().into_iter().find(|s| s.method == method && s.path == path).unwrap_or_else(|| panic!("no {method} {path} in {:#?}", self.seen()))
    }
}

async fn handle(State(f): State<Fake>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    f.seen.lock().unwrap().push(Seen {
        method: method.to_string(),
        path: uri.path().to_string(),
        query: uri.query().unwrap_or("").to_string(),
        auth: h("authorization"),
        accept: h("accept"),
        version: h("x-github-api-version"),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    });
    let routes = f.routes.lock().unwrap().clone();
    // Last matching route wins, so a test can override.
    match routes.iter().rev().find(|(m, p, ..)| *m == method && *p == uri.path()) {
        Some((_, _, _, _, Some(to))) => (StatusCode::FOUND, [("location", to.clone())]).into_response(),
        Some((_, _, st, v, None)) => {
            let status = StatusCode::from_u16(*st).unwrap();
            match v {
                Value::String(raw) if raw.starts_with("RAW:") => (status, raw[4..].to_string()).into_response(),
                v => (status, axum::Json(v.clone())).into_response(),
            }
        }
        None => (StatusCode::NOT_FOUND, axum::Json(json!({"message": "Not Found"}))).into_response(),
    }
}

async fn serve(f: Fake) -> String {
    let app = axum::Router::new().fallback(handle).with_state(f);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

/// A connection to the fake with distinct read and write tokens.
fn conn(kind: ForgeKind, base: &str, api: &str, store: &Arc<MemoryTokens>, read: Option<&str>, write: Option<&str>) -> Arc<dyn Forge> {
    let mut c = Connection::new(kind, base, AuthSource::Token).unwrap();
    c.api_url = Some(api.to_string());
    if let Some(r) = read {
        store.put(&c.id, Purpose::Read, &Secret::new(r)).unwrap();
    }
    if let Some(w) = write {
        store.put(&c.id, Purpose::Write, &Secret::new(w)).unwrap();
    }
    let creds = Arc::new(StoreCredentials::new(store.clone(), c.id.clone(), AuthSource::Token, Some("'/bin/ligus' forge git-credential 'x'".into())));
    c.build(creds)
}

fn draft(base: (&str, &str), head: (&str, &str), branch: &str) -> ChangeRequestDraft {
    ChangeRequestDraft {
        base: RepoRef::new(base.0, base.1),
        base_branch: "main".into(),
        head: RepoRef::new(head.0, head.1),
        head_branch: branch.into(),
        title: "Fix the compiler".into(),
        body: "from Ligus".into(),
        draft: true,
    }
}

fn no_token_in_bodies(f: &Fake, tokens: &[&str]) {
    for s in f.seen() {
        let b = s.body.to_string();
        for t in tokens {
            assert!(!b.contains(t) && !s.query.contains(t) && !s.path.contains(t), "token {t} in a request body/URL: {s:?}");
        }
    }
}

// ── GitHub ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn github_reads_use_the_read_token_and_pin_the_api_version() {
    let f = Fake::default();
    f.on(Method::GET, "/user", 200, json!({"id": 42, "login": "acme", "name": "Acme"}))
        .on(Method::GET, "/user/repos", 200, json!([{"owner": {"login": "acme"}, "name": "widget", "private": true, "default_branch": "main", "fork": false, "html_url": "https://github.com/acme/widget"}]))
        .on(Method::GET, "/user/orgs", 200, json!([{"login": "acme-org"}]))
        .on(Method::GET, "/repos/acme/pipelines", 200, json!({"owner": {"login": "acme"}, "name": "pipelines", "fork": true, "default_branch": "master", "permissions": {"push": true}, "parent": {"owner": {"login": "kubeflow"}, "name": "pipelines", "default_branch": "main"}}));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let gh = conn(ForgeKind::GitHub, "https://github.com", &api, &store, Some("ghr-read"), Some("ghw-write"));
    let me = gh.whoami().await.unwrap().unwrap();
    assert_eq!((me.id.as_str(), me.login.as_str()), ("42", "acme"));
    let s = f.find("GET", "/user");
    assert_eq!(s.auth.as_deref(), Some("Bearer ghr-read"), "reads use the read token");
    assert_eq!(s.version.as_deref(), Some("2022-11-28"));
    assert_eq!(s.accept.as_deref(), Some("application/vnd.github+json"));
    let list = gh.list_repos(&RepoQuery { per_page: 30, page: 1, ..Default::default() }, "acme").await.unwrap();
    assert_eq!(list[0].repo.full_name(), "acme/widget");
    assert!(f.find("GET", "/user/repos").query.contains("affiliation=owner%2Ccollaborator%2Corganization_member"));
    assert_eq!(gh.owners("acme").await.unwrap(), vec!["acme", "acme-org"]);
    let m = gh.repo(&RepoRef::new("acme", "pipelines")).await.unwrap().unwrap();
    assert_eq!(m.parent, Some((RepoRef::new("kubeflow", "pipelines"), "main".into())));
    assert_eq!(m.can_push, Some(true));
    let fork = gh.find_own_fork("acme", &RepoRef::new("kubeflow", "pipelines")).await.unwrap().unwrap();
    assert_eq!(fork.repo.full_name(), "acme/pipelines");
    assert!(gh.repo(&RepoRef::new("ghost", "missing")).await.unwrap().is_none(), "404 is None");
    no_token_in_bodies(&f, &["ghr-read", "ghw-write"]);
}

#[tokio::test]
async fn github_opens_fork_aware_draft_prs_with_the_write_token_and_finds_existing_ones() {
    let f = Fake::default();
    f.on(Method::POST, "/repos/kubeflow/pipelines/pulls", 201, json!({"number": 7, "html_url": "https://github.com/kubeflow/pipelines/pull/7"}));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let gh = conn(ForgeKind::GitHub, "https://github.com", &api, &store, Some("r"), Some("w"));
    let out = gh.open_change_request(&draft(("kubeflow", "pipelines"), ("acme", "pipelines"), "hive/fix-1")).await.unwrap();
    assert_eq!(out, ChangeRequestOutcome::Opened { number: "7".into(), url: "https://github.com/kubeflow/pipelines/pull/7".into(), draft_supported: true, existing: false });
    let s = f.find("POST", "/repos/kubeflow/pipelines/pulls");
    assert_eq!(s.auth.as_deref(), Some("Bearer w"), "writes use the write token");
    assert_eq!(s.body, json!({"title": "Fix the compiler", "body": "from Ligus", "head": "acme:hive/fix-1", "base": "main", "draft": true}));

    // Already open: 422, then the open PR for that head.
    f.on(Method::POST, "/repos/kubeflow/pipelines/pulls", 422, json!({"message": "Validation Failed", "errors": [{"message": "A pull request already exists for acme:hive/fix-1."}]}))
        .on(Method::GET, "/repos/kubeflow/pipelines/pulls", 200, json!([{"number": 7, "html_url": "https://github.com/kubeflow/pipelines/pull/7"}]));
    let out = gh.open_change_request(&draft(("kubeflow", "pipelines"), ("acme", "pipelines"), "hive/fix-1")).await.unwrap();
    assert!(matches!(out, ChangeRequestOutcome::Opened { existing: true, .. }));
    assert!(f.find("GET", "/repos/kubeflow/pipelines/pulls").query.contains("head=acme%3Ahive%2Ffix-1"));

    // A same-org fork names its head repo.
    f.on(Method::POST, "/repos/org/app/pulls", 201, json!({"number": 1, "html_url": "u"}));
    gh.open_change_request(&draft(("org", "app"), ("org", "app-fork"), "b")).await.unwrap();
    let s = f.find("POST", "/repos/org/app/pulls");
    assert_eq!((s.body["head"].clone(), s.body["head_repo"].clone()), (json!("b"), json!("app-fork")));
}

#[tokio::test]
async fn writes_without_a_write_token_never_reach_the_forge() {
    let f = Fake::default();
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    for kind in [ForgeKind::GitHub, ForgeKind::GitLab, ForgeKind::Gitea] {
        let base = match kind {
            ForgeKind::GitHub => "https://github.com",
            ForgeKind::GitLab => "https://gitlab.com",
            ForgeKind::Gitea => "https://codeberg.org",
        };
        let fg = conn(kind, base, &api, &store, Some("read-only"), None);
        let e = fg.open_change_request(&draft(("a", "b"), ("c", "b"), "x")).await.unwrap_err();
        assert!(matches!(e, ForgeError::NotSignedIn(_)), "{kind:?}: {e}");
        assert!(matches!(fg.create_fork(&RepoRef::new("a", "b")).await.unwrap_err(), ForgeError::NotSignedIn(_)));
        assert!(fg.git_auth_args().is_empty(), "no write token, no git credential helper");
    }
    assert!(f.seen().iter().all(|s| s.method == "GET"), "nothing was written: {:?}", f.seen());
}

#[tokio::test]
async fn github_ci_falls_back_to_statuses_when_checks_are_refused() {
    let f = Fake::default();
    f.on(Method::GET, "/repos/a/b/commits/main/status", 200, json!({"statuses": [{"context": "ci/lint", "state": "success", "target_url": "https://ci/1"}, {"context": "ci/test", "state": "pending"}]}))
        .on(Method::GET, "/repos/a/b/commits/main/check-runs", 403, json!({"message": "Resource not accessible by personal access token"}));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let gh = conn(ForgeKind::GitHub, "https://github.com", &api, &store, Some("r"), None);
    let ci = gh.ci_status(&RepoRef::new("a", "b"), "main").await.unwrap();
    assert_eq!(ci.state, CiState::Pending);
    assert_eq!(ci.checks.len(), 2);
    assert!(ci.note.unwrap().contains("checks unavailable with this token"));
    // With checks readable, a failed run makes the whole thing fail.
    f.on(Method::GET, "/repos/a/b/commits/main/check-runs", 200, json!({"check_runs": [{"name": "build", "status": "completed", "conclusion": "failure", "html_url": "https://x"}]}));
    let ci = gh.ci_status(&RepoRef::new("a", "b"), "main").await.unwrap();
    assert_eq!((ci.state, ci.note), (CiState::Failure, None));
}

#[tokio::test]
async fn github_reads_prs_issues_comments_and_files() {
    let f = Fake::default();
    f.on(Method::GET, "/repos/a/b/pulls", 200, json!([{"number": 3, "title": "T", "state": "open", "draft": true, "user": {"login": "u"}, "head": {"label": "u:x", "sha": "abc"}, "base": {"ref": "main"}, "html_url": "h", "merged_at": null}]))
        .on(Method::GET, "/repos/a/b/pulls/3", 200, json!({"number": 3, "title": "T", "state": "closed", "merged_at": "2026-01-01T00:00:00Z", "user": {"login": "u"}, "head": {"label": "u:x"}, "base": {"ref": "main"}}))
        .on(Method::GET, "/repos/a/b/pulls/3/reviews", 200, json!([{"user": {"login": "rev"}, "body": "LGTM", "submitted_at": "2026-01-02"}, {"user": {"login": "rev"}, "body": "", "submitted_at": "2026-01-03"}]))
        .on(Method::GET, "/repos/a/b/pulls/3/comments", 200, json!([{"user": {"login": "rev"}, "body": "nit", "path": "src/x.rs", "line": 12, "created_at": "2026-01-01"}]))
        .on(Method::GET, "/repos/a/b/issues/3/comments", 200, json!([]))
        .on(Method::GET, "/repos/a/b/issues", 200, json!([{"number": 1, "title": "Bug", "state": "open", "user": {"login": "u"}, "labels": [{"name": "bug"}]}, {"number": 3, "pull_request": {}}]))
        .on(Method::GET, "/repos/a/b/contents/src/lib.rs", 200, json!("RAW:fn main() {}\n"));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let gh = conn(ForgeKind::GitHub, "https://github.com", &api, &store, Some("r"), None);
    let r = RepoRef::new("a", "b");
    let prs = gh.list_change_requests(&r, StateFilter::Open, 10).await.unwrap();
    assert_eq!((prs[0].number.as_str(), prs[0].draft, prs[0].head_sha.as_deref()), ("3", true, Some("abc")));
    assert_eq!(gh.change_request(&r, "3").await.unwrap().state, CrState::Merged);
    let c = gh.review_comments(&r, "3").await.unwrap();
    assert_eq!(c.len(), 2, "empty review bodies are skipped: {c:?}");
    assert_eq!((c[0].path.as_deref(), c[0].line), (Some("src/x.rs"), Some(12)));
    let issues = gh.issues(&r, StateFilter::Open, 10).await.unwrap();
    assert_eq!(issues.len(), 1, "pull requests are not issues");
    assert_eq!(issues[0].labels, vec!["bug"]);
    let file = gh.file_at(&r, "src/lib.rs", "main").await.unwrap();
    assert_eq!(file.text, "fn main() {}\n");
    let s = f.find("GET", "/repos/a/b/contents/src/lib.rs");
    assert_eq!((s.accept.as_deref(), s.query.as_str()), (Some("application/vnd.github.raw+json"), "ref=main"));
}

#[tokio::test]
async fn tokens_never_follow_a_redirect_to_another_host() {
    let other = Fake::default();
    other.on(Method::GET, "/user", 200, json!({"id": 1, "login": "x"}));
    let other_url = serve(other.clone()).await;
    let f = Fake::default();
    f.redirect("/user", &format!("{other_url}/user"));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let gh = conn(ForgeKind::GitHub, "https://github.com", &api, &store, Some("secret-r"), None);
    let e = gh.whoami().await.unwrap_err();
    assert!(!e.to_string().contains("secret-r"));
    assert!(other.seen().is_empty(), "the other host saw a request: {:?}", other.seen());
}

#[tokio::test]
async fn errors_never_carry_the_token() {
    let f = Fake::default();
    f.on(Method::GET, "/user", 401, json!({"message": "Bad credentials"}));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let gh = conn(ForgeKind::GitHub, "https://github.com", &api, &store, Some("ghp_SECRETVALUE"), None);
    let e = gh.whoami().await.unwrap_err();
    assert!(matches!(e, ForgeError::Unauthorized(_)));
    assert!(!format!("{e} {e:?}").contains("SECRETVALUE"));
}

// ── GitLab ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn gitlab_cross_fork_draft_mr_is_created_on_the_fork_with_target_project_id() {
    let f = Fake::default();
    f.on(Method::GET, "/projects/upstream%2Fapp", 200, json!({"id": 100, "path_with_namespace": "upstream/app", "default_branch": "main"}))
        .on(Method::GET, "/projects/acme%2Fapp", 200, json!({"id": 200, "path_with_namespace": "acme/app", "default_branch": "main", "forked_from_project": {"path_with_namespace": "upstream/app", "default_branch": "main"}, "permissions": {"project_access": {"access_level": 40}}}))
        .on(Method::POST, "/projects/acme%2Fapp/merge_requests", 201, json!({"iid": 5, "web_url": "https://gitlab.example.com/upstream/app/-/merge_requests/5"}));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let gl = conn(ForgeKind::GitLab, "https://gitlab.example.com", &api, &store, Some("glr"), Some("glw"));
    let m = gl.repo(&RepoRef::new("acme", "app")).await.unwrap().unwrap();
    assert_eq!((m.parent.clone().unwrap().0.full_name(), m.can_push), ("upstream/app".into(), Some(true)));
    let out = gl.open_change_request(&draft(("upstream", "app"), ("acme", "app"), "hive/x")).await.unwrap();
    assert!(matches!(out, ChangeRequestOutcome::Opened { ref number, draft_supported: false, existing: false, .. } if number == "5"));
    let s = f.find("POST", "/projects/acme%2Fapp/merge_requests");
    assert_eq!(s.auth.as_deref(), Some("Bearer glw"));
    assert_eq!(s.body["title"], "Draft: Fix the compiler", "no draft field on create: a Draft: prefix");
    assert_eq!((s.body["target_project_id"].clone(), s.body["source_branch"].clone(), s.body["target_branch"].clone()), (json!(100), json!("hive/x"), json!("main")));
    assert!(s.body.get("draft").is_none());

    // Already open: 409, then the open MR from that source project.
    f.on(Method::POST, "/projects/acme%2Fapp/merge_requests", 409, json!({"message": ["Another open merge request already exists for this source branch: !5"]}))
        .on(Method::GET, "/projects/100/merge_requests", 200, json!([{"iid": 4, "source_branch": "hive/x", "source_project_id": 999, "web_url": "other"}, {"iid": 5, "source_branch": "hive/x", "source_project_id": 200, "web_url": "mine"}]));
    let out = gl.open_change_request(&draft(("upstream", "app"), ("acme", "app"), "hive/x")).await.unwrap();
    assert_eq!(out, ChangeRequestOutcome::Opened { number: "5".into(), url: "mine".into(), draft_supported: false, existing: true });
}

#[tokio::test]
async fn gitlab_reads_discussions_ci_and_files_and_lists_nested_groups() {
    let f = Fake::default();
    f.on(Method::GET, "/projects/grp%2Fsub%2Fapp/merge_requests/2/discussions", 200, json!([{"notes": [{"system": true, "body": "added 1 commit"}, {"author": {"username": "rev"}, "body": "fix this", "position": {"new_path": "a.rs", "new_line": 3}, "resolvable": true, "resolved": false}]}]))
        .on(Method::GET, "/projects/grp%2Fsub%2Fapp/repository/commits/main", 200, json!({"id": "deadbeef", "last_pipeline": {"id": 9, "status": "running", "web_url": "p"}}))
        .on(Method::GET, "/projects/grp%2Fsub%2Fapp/repository/commits/deadbeef/statuses", 200, json!([{"name": "lint", "status": "success"}]))
        .on(Method::GET, "/projects/grp%2Fsub%2Fapp/repository/files/src%2Fa.rs/raw", 200, json!("RAW:x"))
        .on(Method::GET, "/projects", 200, json!([{"path_with_namespace": "grp/sub/app", "visibility": "private", "default_branch": "main"}]))
        .on(Method::GET, "/groups", 200, json!([{"full_path": "grp/sub"}]));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let gl = conn(ForgeKind::GitLab, "https://gitlab.com", &api, &store, Some("r"), None);
    let r = RepoRef::new("grp/sub", "app");
    let c = gl.review_comments(&r, "2").await.unwrap();
    assert_eq!(c.len(), 1, "system notes skipped");
    assert_eq!((c[0].path.as_deref(), c[0].line, c[0].resolved), (Some("a.rs"), Some(3), Some(false)));
    let ci = gl.ci_status(&r, "main").await.unwrap();
    assert_eq!((ci.state, ci.checks.len()), (CiState::Running, 2));
    assert_eq!(gl.file_at(&r, "src/a.rs", "main").await.unwrap().text, "x");
    let l = gl.list_repos(&RepoQuery { q: Some("app".into()), per_page: 20, page: 1, ..Default::default() }, "me").await.unwrap();
    assert_eq!((l[0].repo.owner.as_str(), l[0].private), ("grp/sub", true));
    assert!(f.find("GET", "/projects").query.contains("membership=true") && f.find("GET", "/projects").query.contains("search=app"));
    assert_eq!(gl.owners("me").await.unwrap(), vec!["me", "grp/sub"]);
}

// ── Forgejo / Gitea / Codeberg ─────────────────────────────────────────────

#[tokio::test]
async fn gitea_wip_prs_use_token_auth_and_owner_branch_heads() {
    let f = Fake::default();
    f.on(Method::POST, "/repos/forgejo/forgejo/pulls", 201, json!({"number": 11, "html_url": "https://codeberg.org/forgejo/forgejo/pulls/11"}))
        .on(Method::GET, "/repos/acme/forgejo", 200, json!({"owner": {"login": "acme"}, "name": "forgejo", "fork": true, "default_branch": "forgejo", "permissions": {"push": true}, "parent": {"owner": {"login": "forgejo"}, "name": "forgejo", "default_branch": "forgejo"}}));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let cb = conn(ForgeKind::Gitea, "https://codeberg.org", &api, &store, Some("cbr"), Some("cbw"));
    let m = cb.repo(&RepoRef::new("acme", "forgejo")).await.unwrap().unwrap();
    assert_eq!(m.parent.unwrap().0.full_name(), "forgejo/forgejo");
    assert_eq!(f.find("GET", "/repos/acme/forgejo").auth.as_deref(), Some("token cbr"));
    let out = cb.open_change_request(&draft(("forgejo", "forgejo"), ("acme", "forgejo"), "hive/y")).await.unwrap();
    assert!(matches!(out, ChangeRequestOutcome::Opened { ref number, existing: false, .. } if number == "11"));
    let s = f.find("POST", "/repos/forgejo/forgejo/pulls");
    assert_eq!(s.auth.as_deref(), Some("token cbw"));
    assert_eq!(s.body, json!({"head": "acme:hive/y", "base": "main", "title": "WIP: Fix the compiler", "body": "from Ligus"}));

    // 409: the open PR for that head.
    f.on(Method::POST, "/repos/forgejo/forgejo/pulls", 409, json!({"message": "pull request already exists for these targets [id: 11]"}))
        .on(Method::GET, "/repos/forgejo/forgejo/pulls", 200, json!([{"number": 10, "head": {"ref": "hive/y", "repo": {"full_name": "someone/forgejo"}}, "html_url": "no"}, {"number": 11, "head": {"ref": "hive/y", "repo": {"full_name": "acme/forgejo"}}, "html_url": "yes"}]));
    let out = cb.open_change_request(&draft(("forgejo", "forgejo"), ("acme", "forgejo"), "hive/y")).await.unwrap();
    assert_eq!(out, ChangeRequestOutcome::Opened { number: "11".into(), url: "yes".into(), draft_supported: false, existing: true });
}

#[tokio::test]
async fn gitea_reads_reviews_ci_issues_and_raw_files() {
    let f = Fake::default();
    f.on(Method::GET, "/repos/o/r/pulls/4/reviews", 200, json!([{"id": 77, "user": {"login": "rev"}, "body": "please", "comments_count": 1, "submitted_at": "2026-01-01"}]))
        .on(Method::GET, "/repos/o/r/pulls/4/reviews/77/comments", 200, json!([{"user": {"login": "rev"}, "body": "here", "path": "x.go", "position": 8, "created_at": "2026-01-02"}]))
        .on(Method::GET, "/repos/o/r/issues/4/comments", 200, json!([]))
        .on(Method::GET, "/repos/o/r/commits/main/status", 200, json!({"state": "failure", "statuses": [{"context": "ci", "status": "failure", "target_url": ""}]}))
        .on(Method::GET, "/repos/o/r/issues", 200, json!([{"number": 2, "title": "Bug", "state": "open", "user": {"login": "u"}, "labels": []}]))
        .on(Method::GET, "/repos/o/r/raw/docs/a.md", 200, json!("RAW:# A"))
        .on(Method::GET, "/repos/o/r/pulls", 200, json!([{"number": 4, "title": "WIP: thing", "state": "open", "user": {"login": "u"}, "head": {"ref": "x"}, "base": {"ref": "main"}}]));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let g = conn(ForgeKind::Gitea, "https://git.example.org", &api, &store, Some("r"), None);
    let r = RepoRef::new("o", "r");
    let c = g.review_comments(&r, "4").await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[1].path.as_deref(), Some("x.go"));
    let ci = g.ci_status(&r, "main").await.unwrap();
    assert_eq!((ci.state, ci.checks[0].url.clone()), (CiState::Failure, None));
    assert_eq!(g.issues(&r, StateFilter::Open, 5).await.unwrap()[0].number, "2");
    assert!(f.find("GET", "/repos/o/r/issues").query.contains("type=issues"));
    assert_eq!(g.file_at(&r, "docs/a.md", "main").await.unwrap().text, "# A");
    assert!(g.list_change_requests(&r, StateFilter::Open, 5).await.unwrap()[0].draft, "a WIP: title is a draft");
}

#[tokio::test]
async fn git_auth_args_point_at_the_helper_never_at_the_token() {
    let store = Arc::new(MemoryTokens::default());
    for (kind, base) in [(ForgeKind::GitHub, "https://github.com"), (ForgeKind::GitLab, "https://gitlab.example.com"), (ForgeKind::Gitea, "https://codeberg.org")] {
        let f = conn(kind, base, "http://127.0.0.1:9", &store, None, Some("tok-WRITE-123"));
        let args = f.git_auth_args();
        assert_eq!(args.len(), 4, "{args:?}");
        assert_eq!(args[1], format!("credential.{base}.helper="));
        assert!(args[3].starts_with(&format!("credential.{base}.helper=!")));
        assert!(!args.join(" ").contains("tok-WRITE-123"));
        assert_eq!(f.clone_url(&RepoRef::new("o", "n"), Transport::Https), format!("{base}/o/n.git"));
        assert_eq!(f.repo_of_remote(&format!("{base}/o/n.git")), Some(RepoRef::new("o", "n")));
        assert_eq!(f.repo_of_remote("https://elsewhere.example/o/n.git"), None);
    }
}

/// `forge_token_env_never_reaches_a_child`: agent children get an
/// allow-listed env, and no forge token variable is on the list.
#[test]
fn forge_token_env_never_reaches_a_child() {
    let parent: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        FORGE_TOKEN_ENV.iter().map(|k| ((*k).into(), "tok".into())).chain([("PATH".into(), "/usr/bin".into())]).collect();
    let env = hive_core::process_util::child_env_from(parent, &Default::default());
    let names: Vec<String> = env.iter().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["PATH"], "{names:?}");
    // Even when a caller tries to pass one explicitly.
    let spec = hive_core::process_util::ChildEnvSpec { extra: FORGE_TOKEN_ENV.iter().map(|k| (k.to_string(), "x".into())).collect(), ..Default::default() };
    assert!(hive_core::process_util::child_env_from(Vec::new(), &spec).is_empty());
}

#[test]
fn push_remote_is_the_fork_never_upstream() {
    let ok = RepoLayout { origin: RepoRef::new("me", "p"), upstream: Some(RepoRef::new("kubeflow", "p")) };
    assert_eq!(push_remote(&ok).unwrap(), "origin");
    let bad = RepoLayout { origin: RepoRef::new("Kubeflow", "P"), upstream: Some(RepoRef::new("kubeflow", "p")) };
    assert!(push_remote(&bad).unwrap_err().to_string().contains("refusing to push"));
    let _ = BTreeMap::<u8, u8>::new();
}

// ── Device sign-in ─────────────────────────────────────────────────────────

/// The verification address the desktop opens and the phone shows must be on
/// the code host itself, over https (plain http only to a real loopback
/// host): `http://localhost.evil.example` and `http://127.0.0.1.nip.io` are
/// not loopback, and a page elsewhere is a phishing page.
#[tokio::test]
async fn verification_uri_must_be_on_the_forge_host() {
    for (uri, ok) in [
        ("http://localhost.evil.example/device", false),
        ("http://127.0.0.1.nip.io/device", false),
        ("https://evil.example/device", false),
        ("javascript:alert(1)", false),
        ("http://127.0.0.1/oauth/device", true),
    ] {
        let f = Fake::default();
        f.on(Method::POST, "/oauth/authorize_device", 200, json!({"device_code": "DEV", "user_code": "ABCD-1234", "verification_uri": uri}));
        let base = serve(f.clone()).await;
        let mut c = Connection::new(ForgeKind::GitLab, &base, AuthSource::Oauth).unwrap();
        c.client_id = Some("app-123".into());
        let ep = hive_forge::device::endpoints(&c).unwrap();
        assert_eq!(hive_forge::device::start(&ep).await.is_ok(), ok, "{uri}");
    }
}

#[tokio::test]
async fn device_flow_starts_polls_and_refreshes_without_a_client_secret() {
    let f = Fake::default();
    f.on(Method::POST, "/oauth/authorize_device", 200, json!({"device_code": "DEV", "user_code": "ABCD-1234", "verification_uri": "http://127.0.0.1/oauth/device", "expires_in": 300, "interval": 5}))
        .on(Method::POST, "/oauth/token", 400, json!({"error": "authorization_pending"}));
    let base = serve(f.clone()).await;
    let mut c = Connection::new(ForgeKind::GitLab, &base, AuthSource::Oauth).unwrap();
    assert!(hive_forge::device::endpoints(&c).is_err(), "self-managed GitLab needs its own application id");
    c.client_id = Some("app-123".into());
    let ep = hive_forge::device::endpoints(&c).unwrap();
    let st = hive_forge::device::start(&ep).await.unwrap();
    assert_eq!((st.user_code.as_str(), st.interval), ("ABCD-1234", 5));
    let s = f.find("POST", "/oauth/authorize_device");
    assert_eq!(s.accept.as_deref(), Some("application/json"));
    assert!(matches!(hive_forge::device::poll(&ep, &st.device_code).await.unwrap(), hive_forge::device::Poll::Pending));
    f.on(Method::POST, "/oauth/token", 400, json!({"error": "slow_down", "interval": 10}));
    assert!(matches!(hive_forge::device::poll(&ep, &st.device_code).await.unwrap(), hive_forge::device::Poll::SlowDown(10)));
    f.on(Method::POST, "/oauth/token", 200, json!({"access_token": "AT", "refresh_token": "RT", "expires_in": 7200}));
    let hive_forge::device::Poll::Done(t) = hive_forge::device::poll(&ep, &st.device_code).await.unwrap() else { panic!("not done") };
    assert_eq!((t.access.expose(), t.refresh.as_ref().map(|r| r.expose().to_string()), t.expires_in), ("AT", Some("RT".into()), Some(7200)));
    let r = hive_forge::device::refresh(&ep.token_url, &ep.client_id, &Secret::new("RT")).await.unwrap();
    assert_eq!(r.access.expose(), "AT");
    for s in f.seen() {
        assert!(s.body.is_null(), "form posts only");
        assert!(s.auth.is_none(), "no client secret, no auth header");
    }
    // Forgejo/Gitea have no device flow.
    let g = Connection::new(ForgeKind::Gitea, "https://codeberg.org", AuthSource::Token).unwrap();
    assert!(matches!(hive_forge::device::endpoints(&g), Err(ForgeError::Unsupported(_))));
}

#[tokio::test]
async fn an_expired_oauth_token_is_refreshed_from_the_store_before_use() {
    let f = Fake::default();
    f.on(Method::POST, "/login/oauth/access_token", 200, json!({"access_token": "NEW", "refresh_token": "RT2", "expires_in": 28800}))
        .on(Method::GET, "/api/v3/user", 200, json!({"id": 1, "login": "me"}));
    let base = serve(f.clone()).await;
    let mut c = Connection::new(ForgeKind::GitHub, &base, AuthSource::Oauth).unwrap();
    c.client_id = Some("Iv1.app".into());
    c.token_expires_at = Some(1);
    let store = Arc::new(MemoryTokens::default());
    store.put(&c.id, Purpose::Write, &Secret::new("OLD")).unwrap();
    store.put(&c.id, Purpose::Refresh, &Secret::new("RT")).unwrap();
    let gh = c.build(c.credentials(store.clone(), None, None));
    assert_eq!(gh.whoami().await.unwrap().unwrap().login, "me");
    assert_eq!(f.find("GET", "/api/v3/user").auth.as_deref(), Some("Bearer NEW"));
    assert_eq!(store.get(&c.id, Purpose::Refresh).unwrap().unwrap().expose(), "RT2");
}

/// Gitea and Forgejo echo a bad token in their error body ("access token
/// does not exist [sha: …]"): the error Ligus builds never carries it.
#[tokio::test]
async fn a_token_the_forge_echoes_back_is_redacted() {
    let f = Fake::default();
    f.on(Method::GET, "/user", 401, json!({"message": "token is malformed: access token does not exist [sha: cb-ECHOED-0123456789]"}));
    let api = serve(f.clone()).await;
    let store = Arc::new(MemoryTokens::default());
    let g = conn(ForgeKind::Gitea, "https://codeberg.org", &api, &store, Some("cb-ECHOED-0123456789"), None);
    let e = g.whoami().await.unwrap_err();
    assert!(!e.to_string().contains("cb-ECHOED"), "{e}");
    assert!(e.to_string().contains("***"));
}

/// A hostile or broken code host that streams an endless answer to a device
/// sign-in or refresh: the read stops at the cap, it doesn't buffer it all.
#[tokio::test]
async fn device_answers_are_read_with_a_cap() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n").await;
                let chunk = format!("{:x}\r\n{}\r\n", 16 * 1024, " ".repeat(16 * 1024));
                while s.write_all(chunk.as_bytes()).await.is_ok() {}
            });
        }
    });
    let ep = hive_forge::device::DeviceEndpoints {
        device_url: format!("http://{addr}/login/device/code"),
        token_url: format!("http://{addr}/login/oauth/access_token"),
        client_id: "x".into(),
        scope: None,
    };
    let r = tokio::time::timeout(std::time::Duration::from_secs(5), hive_forge::device::start(&ep)).await;
    let err = r.expect("the read stops at the cap instead of draining an endless body").unwrap_err();
    assert!(err.to_string().contains("too large"), "{err}");
}

/// GitLab rotates refresh tokens: a refresh token works once. `ligus serve`,
/// an agent's read tools and the git credential helper each build their own
/// credentials from `forges.toml`. Only one of them may refresh, the new
/// expiry must land in `forges.toml` (so the next process doesn't refresh
/// again with a spent refresh token), and it is written once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refreshed_expiry_is_persisted_once() {
    let f = Fake::default();
    f.on(Method::POST, "/oauth/token", 200, json!({"access_token": "NEW", "refresh_token": "RT2", "expires_in": 7200}));
    let base = serve(f.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut c = Connection::new(ForgeKind::GitLab, &base, AuthSource::Oauth).unwrap();
    c.client_id = Some("app-123".into());
    c.token_expires_at = Some(1);
    let mut all = Connections::default();
    all.upsert(c.clone());
    all.save(dir.path()).unwrap();
    let store = Arc::new(MemoryTokens::default());
    store.put(&c.id, Purpose::Write, &Secret::new("OLD")).unwrap();
    store.put(&c.id, Purpose::Refresh, &Secret::new("RT")).unwrap();
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let conn = Connections::load(dir.path()).get(&c.id).cloned().unwrap();
        let creds = conn.credentials(store.clone(), None, Some(dir.path()));
        tasks.push(tokio::spawn(async move { creds.token(Purpose::Write).await.unwrap().unwrap().expose().to_string() }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap(), "NEW");
    }
    let refreshes = || f.seen().iter().filter(|s| s.method == "POST" && s.path == "/oauth/token").count();
    assert_eq!(refreshes(), 1, "one refresh across processes: a second one would spend the rotated refresh token");
    let saved = Connections::load(dir.path()).get(&c.id).and_then(|c| c.token_expires_at).unwrap_or(0);
    assert!(saved > hive_forge::device::now_unix() + 7000, "the refreshed expiry is in forges.toml: {saved}");
    // The next process reads the fresh expiry and doesn't refresh again.
    let conn = Connections::load(dir.path()).get(&c.id).cloned().unwrap();
    let late = conn.credentials(store.clone(), None, Some(dir.path()));
    assert_eq!(late.token(Purpose::Write).await.unwrap().unwrap().expose(), "NEW");
    assert_eq!(refreshes(), 1);
}

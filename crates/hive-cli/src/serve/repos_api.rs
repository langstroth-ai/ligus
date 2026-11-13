//! Repos on the home host, from the phone: list the box's checkouts and the
//! GitHub repos its credentials can see, clone one on demand, switch which
//! checkout a chat works in, pull the base branch, and open a pull request
//! from the chat's branch.
//!
//! Everything here acts on this box's own device-local state
//! (`<HIVE_DATA_DIR>/repo-bindings.toml` and the checkouts under
//! `HIVE_REPOS_DIR`, see `crate::repos`); nothing is synced. GitHub access goes
//! through the box's own `gh` login (never a token Hive holds), behind the
//! [`GitHub`] trait so tests run without the network.
//!
//! Routes (contract in `docs/ios-app.md`):
//! - `GET  /v1/repos?q=&owner=&page=&perPage=`
//! - `GET  /v1/repos/fork?repo=owner/name` → the signed-in account's fork of it
//! - `POST /v1/repos/clone {repo, viaFork?}` → a job; `GET /v1/repos/clone/:job`
//! - `GET|PUT /v1/chats/:id/repo {repo}`
//! - `POST /v1/chats/:id/repo/pull`
//! - `POST /v1/chats/:id/pr {title, body?, draft?}`
//!
//! Forks: a checkout whose `origin` is the owner's fork and whose `upstream`
//! remote is the parent project (see [`repos::fork_layout`]) bases chat
//! branches on upstream's default branch, pulls from upstream, and opens PRs
//! upstream from a readable branch pushed to the fork. Nothing here ever
//! pushes to the parent project or creates a fork.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path as FsPath, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use hive_core::{ChatSession, Timestamp, WorkspaceRole};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::api::{iso, load_chat, ApiError, ApiResult};
use super::Shared;
use crate::repos::{self, CheckoutInfo, PrBranches, RemoteRef, RepoBindings, UPSTREAM_REMOTE};

type S = State<Arc<Shared>>;

/// The role the box's identity needs to change repo state from the phone —
/// the content tier, the same floor as voting on a proposal.
const WRITE_FLOOR: WorkspaceRole = WorkspaceRole::Contributor;
/// At most this many clones/fetches at once.
const MAX_JOBS: usize = 2;
/// Finished jobs kept for polling.
const KEEP_JOBS: usize = 20;

fn env_secs(name: &str, default: u64) -> Duration {
    Duration::from_secs(crate::env_opt(name).and_then(|v| v.trim().parse().ok()).unwrap_or(default))
}

fn clone_timeout() -> Duration {
    env_secs("HIVE_REPOS_CLONE_TIMEOUT_SECS", 900)
}

fn max_clone_bytes() -> u64 {
    crate::env_opt("HIVE_REPOS_MAX_CLONE_MB").and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(2048) * 1024 * 1024
}

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const GH_TIMEOUT: Duration = Duration::from_secs(30);

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError::new(code, msg)
}

// ── Running commands with a deadline ───────────────────────────────────────

#[derive(Debug, Default)]
pub struct RunOut {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// A command with no terminal prompts (git or gh asking for a password on a
/// headless box would hang the job forever).
pub fn quiet(program: &str) -> Command {
    let mut c = hive_core::process_util::command(program);
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null());
    c
}

/// Run `cmd` to completion or until `timeout`, feeding each stderr line
/// (split on `\r` too, for git's progress) to `on_line`, and killing it when
/// `check` returns a reason to stop.
pub fn run_watched(
    mut cmd: Command,
    timeout: Duration,
    mut on_line: impl FnMut(&str),
    mut check: impl FnMut() -> Option<String>,
) -> Result<RunOut, String> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let (tx, rx) = mpsc::channel::<String>();
    let err_t = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut line = Vec::new();
        loop {
            match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if b == b'\n' || b == b'\r' {
                            if !line.is_empty() {
                                let _ = tx.send(String::from_utf8_lossy(&line).into_owned());
                                line.clear();
                            }
                        } else {
                            line.push(b);
                        }
                    }
                }
            }
        }
        if !line.is_empty() {
            let _ = tx.send(String::from_utf8_lossy(&line).into_owned());
        }
    });
    let start = Instant::now();
    let mut lines: Vec<String> = Vec::new();
    let mut last_check = Instant::now();
    let status = loop {
        while let Ok(l) = rx.try_recv() {
            on_line(&l);
            lines.push(l);
            if lines.len() > 200 {
                lines.drain(..100);
            }
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timed out after {}s", timeout.as_secs()));
        }
        if last_check.elapsed() > Duration::from_millis(500) {
            last_check = Instant::now();
            if let Some(why) = check() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(why);
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    let _ = err_t.join();
    while let Ok(l) = rx.try_recv() {
        on_line(&l);
        lines.push(l);
    }
    let stdout = out_t.join().unwrap_or_default();
    Ok(RunOut { ok: status.success(), stdout, stderr: lines.join("\n") })
}

pub fn run(cmd: Command, timeout: Duration) -> Result<RunOut, String> {
    run_watched(cmd, timeout, |_| {}, || None)
}

/// `git -C dir [auth] <args>`, failing with git's own message.
fn git(dir: &FsPath, auth: &[String], args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut c = quiet("git");
    c.arg("-C").arg(dir).args(auth).args(args);
    let out = run(c, timeout)?;
    if !out.ok {
        return Err(last_lines(&out.stderr));
    }
    Ok(out.stdout.trim().to_string())
}

/// The tail of a command's stderr, for an error message.
fn last_lines(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" ")
}

// ── GitHub (the box's own gh login) ────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhStatus {
    pub available: bool,
    pub login: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RepoQuery {
    pub q: Option<String>,
    pub owner: Option<String>,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GhRepo {
    pub owner: String,
    pub name: String,
    pub description: String,
    pub private: bool,
    pub default_branch: String,
    pub updated_at: String,
    pub url: String,
    /// A fork of another repository.
    pub fork: bool,
}

/// One repository's fork relationship, from GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMeta {
    pub owner: String,
    pub name: String,
    pub default_branch: String,
    /// The repository this one is a fork of: (owner, name, default branch).
    pub parent: Option<(String, String, String)>,
}

#[derive(Debug, Clone)]
pub struct PrRequest {
    pub repo: String,
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// What the box can do on GitHub. The real one shells out to `gh`; tests
/// substitute local bare repos.
pub trait GitHub: Send + Sync {
    fn status(&self) -> GhStatus;
    /// One page of repositories (`q` searches; `owner` narrows to an account
    /// or org). Only called when [`GitHub::status`] is available.
    fn list(&self, q: &RepoQuery, login: &str) -> Result<Vec<GhRepo>, String>;
    /// The signed-in account and its orgs.
    fn owners(&self, login: &str) -> Result<Vec<String>, String>;
    /// Where `owner/name` clones from.
    fn clone_url(&self, owner: &str, name: &str) -> String {
        format!("https://github.com/{owner}/{name}.git")
    }
    /// `git -c …` args that let git use the box's login (none when signed out).
    fn git_auth_args(&self) -> Vec<String>;
    /// The GitHub repository a checkout's `origin` URL points at.
    fn repo_of_remote(&self, url: &str) -> Option<RemoteRef> {
        repos::parse_remote(url).filter(|r| r.host == "github.com")
    }
    /// Open a pull request; `Ok((url, already_existed))`.
    fn create_pr(&self, dir: &FsPath, pr: &PrRequest) -> Result<(String, bool), String>;
    /// `owner/name`'s fork relationship; `Ok(None)` when there's no such repo.
    fn repo_meta(&self, _owner: &str, _name: &str) -> Result<Option<RepoMeta>, String> {
        Ok(None)
    }
}

/// The `gh` CLI on this box.
#[derive(Default)]
pub struct GhCli {
    cache: Mutex<Option<(Instant, GhStatus)>>,
}

impl GhCli {
    fn gh(&self, args: &[&str]) -> Result<String, String> {
        let mut c = quiet("gh");
        c.args(args);
        let out = run(c, GH_TIMEOUT).map_err(|e| format!("gh: {e}"))?;
        if !out.ok {
            return Err(format!("gh: {}", last_lines(&out.stderr)));
        }
        Ok(out.stdout)
    }

    fn api(&self, path: &str, params: &[(&str, String)]) -> Result<Value, String> {
        let mut args: Vec<String> = vec!["api".into(), "-X".into(), "GET".into(), path.into()];
        for (k, v) in params {
            args.push("-f".into());
            args.push(format!("{k}={v}"));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        serde_json::from_str(&self.gh(&refs)?).map_err(|e| format!("gh api: {e}"))
    }

    fn probe(&self) -> GhStatus {
        let mut c = quiet("gh");
        c.args(["auth", "status", "--hostname", "github.com"]);
        match run(c, GH_TIMEOUT) {
            Err(_) => GhStatus {
                available: false,
                login: None,
                reason: Some("Install the GitHub CLI (gh) on the host, then run gh auth login on the host".into()),
            },
            Ok(o) if !o.ok => {
                GhStatus { available: false, login: None, reason: Some("Run gh auth login on the host".into()) }
            }
            Ok(_) => match self.gh(&["api", "user", "--jq", ".login"]) {
                Ok(l) if !l.trim().is_empty() => GhStatus { available: true, login: Some(l.trim().to_string()), reason: None },
                _ => GhStatus {
                    available: false,
                    login: None,
                    reason: Some("GitHub didn't answer with the host's login; run gh auth login on the host".into()),
                },
            },
        }
    }
}

fn gh_repo(v: &Value) -> Option<GhRepo> {
    Some(GhRepo {
        owner: v["owner"]["login"].as_str()?.to_string(),
        name: v["name"].as_str()?.to_string(),
        description: v["description"].as_str().unwrap_or("").to_string(),
        private: v["private"].as_bool().unwrap_or(false),
        default_branch: v["default_branch"].as_str().unwrap_or("").to_string(),
        updated_at: v["pushed_at"].as_str().or(v["updated_at"].as_str()).unwrap_or("").to_string(),
        url: v["html_url"].as_str().unwrap_or("").to_string(),
        fork: v["fork"].as_bool().unwrap_or(false),
    })
}

fn repo_meta_of(v: &Value) -> Option<RepoMeta> {
    let parent = v["parent"].as_object().and_then(|p| {
        Some((
            p.get("owner")?.get("login")?.as_str()?.to_string(),
            p.get("name")?.as_str()?.to_string(),
            p.get("default_branch").and_then(Value::as_str).unwrap_or("").to_string(),
        ))
    });
    Some(RepoMeta {
        owner: v["owner"]["login"].as_str()?.to_string(),
        name: v["name"].as_str()?.to_string(),
        default_branch: v["default_branch"].as_str().unwrap_or("").to_string(),
        parent: parent.filter(|_| v["fork"].as_bool().unwrap_or(false)),
    })
}

impl GitHub for GhCli {
    fn status(&self) -> GhStatus {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, st)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(60) {
                return st.clone();
            }
        }
        let st = self.probe();
        *cache = Some((Instant::now(), st.clone()));
        st
    }

    fn list(&self, q: &RepoQuery, login: &str) -> Result<Vec<GhRepo>, String> {
        let page = [("per_page", q.per_page.to_string()), ("page", q.page.to_string())];
        let items: Vec<Value> = if let Some(text) = q.q.as_deref() {
            let scope = q.owner.as_deref().unwrap_or(login);
            let mut p = page.to_vec();
            p.push(("q", format!("{text} user:{scope} fork:true")));
            p.push(("sort", "updated".into()));
            let v = self.api("search/repositories", &p)?;
            v["items"].as_array().cloned().unwrap_or_default()
        } else {
            let mut p = page.to_vec();
            p.push(("sort", "updated".into()));
            let v = match q.owner.as_deref() {
                None => {
                    p.push(("affiliation", "owner,collaborator,organization_member".into()));
                    self.api("user/repos", &p)?
                }
                Some(o) if o.eq_ignore_ascii_case(login) => {
                    p.push(("affiliation", "owner".into()));
                    self.api("user/repos", &p)?
                }
                Some(o) => self.api(&format!("orgs/{o}/repos"), &p).or_else(|_| self.api(&format!("users/{o}/repos"), &p))?,
            };
            v.as_array().cloned().unwrap_or_default()
        };
        Ok(items.iter().filter_map(gh_repo).collect())
    }

    fn owners(&self, login: &str) -> Result<Vec<String>, String> {
        let orgs = self.api("user/orgs", &[("per_page", "100".into())])?;
        let mut out = vec![login.to_string()];
        out.extend(orgs.as_array().into_iter().flatten().filter_map(|o| o["login"].as_str().map(str::to_string)));
        Ok(out)
    }

    fn git_auth_args(&self) -> Vec<String> {
        if !self.status().available {
            return Vec::new();
        }
        // Reset any inherited helper for github.com, then use gh's.
        ["-c", "credential.https://github.com.helper=", "-c", "credential.https://github.com.helper=!gh auth git-credential"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn create_pr(&self, dir: &FsPath, pr: &PrRequest) -> Result<(String, bool), String> {
        let mut c = quiet("gh");
        c.current_dir(dir).args([
            "pr", "create", "--repo", &pr.repo, "--head", &pr.head, "--base", &pr.base, "--title", &pr.title, "--body", &pr.body,
        ]);
        if pr.draft {
            c.arg("--draft");
        }
        let out = run(c, Duration::from_secs(60))?;
        let url_in = |s: &str| s.split_whitespace().find(|w| w.starts_with("https://github.com/")).map(str::to_string);
        if out.ok {
            return url_in(&out.stdout).map(|u| (u, false)).ok_or_else(|| "gh pr create printed no URL".into());
        }
        if out.stderr.contains("already exists") {
            if let Some(u) = url_in(&out.stderr) {
                return Ok((u, true));
            }
        }
        Err(format!("gh pr create: {}", last_lines(&out.stderr)))
    }

    fn repo_meta(&self, owner: &str, name: &str) -> Result<Option<RepoMeta>, String> {
        match self.api(&format!("repos/{owner}/{name}"), &[]) {
            Ok(v) => Ok(repo_meta_of(&v)),
            Err(e) if e.contains("Not Found") || e.contains("404") => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ── Shared state: GitHub, clone jobs, per-checkout locks ───────────────────

#[derive(Debug, Clone)]
pub struct CloneJob {
    pub id: Uuid,
    pub repo: String,
    pub checkout: String,
    /// `clone`, or `fetch` when the checkout was already there.
    pub action: &'static str,
    /// `running`, `done` or `error`.
    pub state: &'static str,
    pub phase: String,
    pub percent: Option<u8>,
    pub error: Option<String>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

impl CloneJob {
    fn dto(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "repo": self.repo,
            "checkout": self.checkout,
            "action": self.action,
            "state": self.state,
            "phase": self.phase,
            "startedAt": iso(self.started_at),
        });
        if let Some(p) = self.percent {
            v["percent"] = json!(p);
        }
        if let Some(e) = &self.error {
            v["error"] = json!(e);
        }
        if let Some(t) = self.finished_at {
            v["finishedAt"] = json!(iso(t));
        }
        v
    }
}

pub struct RepoState {
    github: RwLock<Arc<dyn GitHub>>,
    jobs: Mutex<Vec<CloneJob>>,
    /// Checkout names with a git operation in flight.
    busy: Mutex<HashSet<String>>,
    /// Serializes read-modify-write of the bindings file.
    bindings: Mutex<()>,
}

impl Default for RepoState {
    fn default() -> Self {
        Self {
            github: RwLock::new(Arc::new(GhCli::default())),
            jobs: Mutex::new(Vec::new()),
            busy: Mutex::new(HashSet::new()),
            bindings: Mutex::new(()),
        }
    }
}

impl RepoState {
    pub fn github(&self) -> Arc<dyn GitHub> {
        self.github.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Swap the GitHub backend (tests).
    #[cfg(test)]
    pub fn set_github(&self, gh: Arc<dyn GitHub>) {
        *self.github.write().unwrap_or_else(|e| e.into_inner()) = gh;
    }

    fn job(&self, id: Uuid) -> Option<CloneJob> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|j| j.id == id).cloned()
    }

    fn update_job(&self, id: Uuid, f: impl FnOnce(&mut CloneJob)) -> Option<CloneJob> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let j = jobs.iter_mut().find(|j| j.id == id)?;
        f(j);
        Some(j.clone())
    }

    fn running_jobs(&self) -> Vec<CloneJob> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|j| j.state == "running").cloned().collect()
    }
}

/// Holds a checkout's lock for one git operation; released on drop.
struct BusyGuard<'a>(&'a RepoState, String);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.busy.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
    }
}

fn claim<'a>(st: &'a RepoState, name: &str) -> ApiResult<BusyGuard<'a>> {
    if !st.busy.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string()) {
        return Err(err(StatusCode::CONFLICT, format!("{name} is busy with another git operation; try again in a moment")));
    }
    Ok(BusyGuard(st, name.to_string()))
}

// ── Authorization ──────────────────────────────────────────────────────────

/// The phone may change repo state only while this box can write at all
/// (not revoked or pending) and its identity holds at least
/// [`WRITE_FLOOR`] in the workspace (the chat's roster when one is given).
fn ensure_can_write(s: &Shared, chat: Option<&ChatSession>) -> ApiResult<()> {
    let me = s.my_actor_id();
    if let Some(reason) = s.lock().write_block() {
        return Err(err(StatusCode::FORBIDDEN, reason.to_string()));
    }
    let role = match chat {
        Some(c) => c.role_of(&me),
        None => super::api::hosts_and_agents(s)?.1.role_of(&me),
    };
    if role.rank() < WRITE_FLOOR.rank() {
        return Err(err(StatusCode::FORBIDDEN, "unauthorized: this host's role in the workspace can't change repositories"));
    }
    Ok(())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ── DTOs ───────────────────────────────────────────────────────────────────

fn remote_dto(r: &RemoteRef) -> Value {
    json!({ "host": r.host, "owner": r.owner, "name": r.name, "fullName": r.full_name() })
}

fn checkout_dto(name: &str, info: &CheckoutInfo) -> Value {
    let mut v = json!({
        "name": name,
        "displayName": info.remote.as_ref().filter(|r| !r.host.is_empty()).map(RemoteRef::full_name).unwrap_or_else(|| name.to_string()),
        "dirty": info.dirty,
        "remote": info.remote.as_ref().map(remote_dto),
    });
    if let Some(b) = &info.default_branch {
        v["defaultBranch"] = json!(b);
    }
    if let Some(b) = &info.branch {
        v["branch"] = json!(b);
    }
    if let Some(sha) = &info.sha {
        v["head"] = json!(sha);
    }
    v
}

/// The phone's view of a fork checkout: the parent project, the base branch,
/// and how the local base compares with upstream's (as of the last fetch).
fn fork_dto(dir: &FsPath, f: &repos::ForkLayout) -> Value {
    let mut v = json!({
        "upstream": f.upstream.full_name(),
        "upstreamRemote": UPSTREAM_REMOTE,
        "baseBranch": f.base_branch,
        "upstreamPushDisabled": f.push_disabled,
        "prTarget": "upstream",
    });
    let local = format!("refs/heads/{}", f.base_branch);
    if repos::ref_exists(dir, &local) && repos::ref_exists(dir, &f.base_ref()) {
        if let Some((behind, ahead)) = repos::behind_ahead(dir, &f.base_ref(), &local) {
            v["behind"] = json!(behind);
            v["ahead"] = json!(ahead);
        }
    }
    v
}

fn checkout_dto_at(dir: &FsPath, name: &str, info: &CheckoutInfo) -> Value {
    let mut v = checkout_dto(name, info);
    if let Some(f) = &info.fork {
        v["fork"] = fork_dto(dir, f);
    }
    v
}

/// The checkouts directly under the repos dir (hidden and in-progress clones
/// skipped), by name.
fn local_checkouts(repos_dir: &FsPath) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(repos_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            (repos::valid_checkout_dir_name(&name) && path.is_dir() && path.join(".git").exists()).then_some((name, path))
        })
        .collect();
    out.sort_by_key(|(n, _)| n.to_ascii_lowercase());
    out
}

/// A raw binding's display name: the name under the repos dir, or the last
/// component of an absolute path (which is never shown).
fn binding_name(raw: &str) -> (String, bool) {
    let p = FsPath::new(raw.trim());
    if p.is_absolute() {
        (p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "checkout".into()), true)
    } else {
        (raw.trim().to_string(), false)
    }
}

/// A chat's binding: the checkout it resolves to and the phone's view of it.
fn chat_binding(data_dir: &FsPath, chat: Uuid) -> (Option<PathBuf>, Value) {
    let b = RepoBindings::load(data_dir);
    let repos_dir = repos::repos_dir(data_dir);
    let workspace_default = b.workspace.as_deref().map(|w| binding_name(w).0);
    let Some((raw, own)) = b.raw_for(chat) else {
        return (None, json!({ "repo": null, "workspaceDefault": workspace_default, "chatBranch": repos::chat_branch(chat) }));
    };
    let (name, external) = binding_name(raw);
    let dir = repos::resolve_path(&repos_dir, raw);
    let branch = repos::chat_branch(chat);
    if !hive_runtime::git_worktree::is_git_repo(&dir) {
        let v = json!({
            "repo": { "name": name, "displayName": name, "source": if own { "chat" } else { "workspace" }, "available": false, "external": external },
            "workspaceDefault": workspace_default,
            "chatBranch": branch,
        });
        return (None, v);
    }
    let info = repos::inspect(&dir);
    let mut repo = checkout_dto_at(&dir, &name, &info);
    repo["source"] = json!(if own { "chat" } else { "workspace" });
    repo["available"] = json!(true);
    repo["external"] = json!(external);
    let exists = repos::git_out(&dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_some();
    let mut out = json!({ "repo": repo, "workspaceDefault": workspace_default, "chatBranch": branch, "chatBranchExists": exists });
    if exists {
        if let Some(base) = info.default_branch.as_deref() {
            if let Some((behind, ahead)) = ahead_behind(&dir, base, &branch, info.fork.is_some()) {
                out["ahead"] = json!(ahead);
                out["behind"] = json!(behind);
            }
        }
    }
    if let Some(b) = PrBranches::load(data_dir).get(&name, chat) {
        out["prBranch"] = json!(b);
    }
    (Some(dir), out)
}

/// (behind, ahead) of `branch` relative to `base`: upstream's in a fork, else
/// the local branch, else origin's.
fn ahead_behind(dir: &FsPath, base: &str, branch: &str, fork: bool) -> Option<(u64, u64)> {
    let upstream = fork.then(|| format!("refs/remotes/{UPSTREAM_REMOTE}/{base}"));
    let base_ref = upstream
        .into_iter()
        .chain([format!("refs/heads/{base}"), format!("refs/remotes/origin/{base}")])
        .find(|r| repos::ref_exists(dir, r))?;
    repos::behind_ahead(dir, &base_ref, &format!("refs/heads/{branch}"))
}

/// `repo` for `GET /v1/chats/:id/details`.
pub fn details_repo(s: &Shared, chat: Uuid) -> Value {
    chat_binding(&s.cfg.data_dir, chat).1
}

// ── GET /v1/repos ──────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    q: Option<String>,
    owner: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
}

pub async fn list_repos(State(s): S, Query(lq): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let q = lq.q.map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
    if q.as_ref().is_some_and(|q| q.chars().count() > 100 || q.chars().any(char::is_control)) {
        return Err(ApiError::bad_request("search text is too long"));
    }
    let owner = lq.owner.map(|o| o.trim().to_string()).filter(|o| !o.is_empty());
    if owner.as_deref().is_some_and(|o| !repos::valid_owner(o)) {
        return Err(ApiError::bad_request("owner must be a GitHub account or org name"));
    }
    let query = RepoQuery { q, owner, page: lq.page.unwrap_or(1).clamp(1, 100), per_page: lq.per_page.unwrap_or(30).clamp(1, 100) };
    let data_dir = s.cfg.data_dir.clone();
    let gh = s.repos.github();
    let jobs: Vec<Value> = s.repos.running_jobs().iter().map(CloneJob::dto).collect();
    let v = blocking(move || {
        let repos_dir = repos::repos_dir(&data_dir);
        let b = RepoBindings::load(&data_dir);
        let ws_default = b.workspace.as_deref().map(|w| binding_name(w).0);
        let needle = query.q.as_deref().map(str::to_ascii_lowercase);
        let mut cloned: HashMap<String, String> = HashMap::new();
        // upstream full name → the local fork checkout of it.
        let mut via_fork: HashMap<String, String> = HashMap::new();
        let local: Vec<Value> = local_checkouts(&repos_dir)
            .into_iter()
            .filter_map(|(name, path)| {
                let info = repos::inspect(&path);
                if let Some(r) = info.remote.as_ref() {
                    cloned.entry(r.full_name().to_ascii_lowercase()).or_insert_with(|| name.clone());
                }
                if let Some(f) = info.fork.as_ref() {
                    via_fork.entry(f.upstream.full_name().to_ascii_lowercase()).or_insert_with(|| name.clone());
                }
                let mut v = checkout_dto_at(&path, &name, &info);
                if let Some(n) = needle.as_deref() {
                    let hay = format!("{} {}", name, v["displayName"].as_str().unwrap_or("")).to_ascii_lowercase();
                    if !hay.contains(n) {
                        return None;
                    }
                }
                v["isWorkspaceDefault"] = json!(ws_default.as_deref() == Some(name.as_str()));
                v["boundChats"] = json!(b.chats.values().filter(|c| c.trim() == name).count());
                Some(v)
            })
            .collect();
        let st = gh.status();
        let mut github = json!({ "available": st.available });
        if let Some(r) = &st.reason {
            github["reason"] = json!(r);
        }
        if let (true, Some(login)) = (st.available, st.login.as_deref()) {
            github["login"] = json!(login);
            match gh.owners(login) {
                Ok(o) => github["owners"] = json!(o),
                Err(_) => github["owners"] = json!([login]),
            }
            match gh.list(&query, login) {
                Ok(list) => {
                    let has_more = list.len() as u32 >= query.per_page;
                    github["repos"] = json!(list
                        .iter()
                        .map(|r| {
                            let full = format!("{}/{}", r.owner, r.name);
                            let mut v = json!({
                                "fullName": full,
                                "owner": r.owner,
                                "name": r.name,
                                "description": r.description,
                                "private": r.private,
                                "defaultBranch": r.default_branch,
                                "updatedAt": r.updated_at,
                                "url": r.url,
                                "fork": r.fork,
                            });
                            if let Some(local) = via_fork.get(&full.to_ascii_lowercase()) {
                                v["viaForkCheckout"] = json!(local);
                            }
                            match cloned.get(&full.to_ascii_lowercase()) {
                                Some(local) => {
                                    v["cloned"] = json!(true);
                                    v["checkout"] = json!(local);
                                }
                                None => v["cloned"] = json!(false),
                            }
                            v
                        })
                        .collect::<Vec<_>>());
                    github["page"] = json!(query.page);
                    github["perPage"] = json!(query.per_page);
                    github["hasMore"] = json!(has_more);
                }
                Err(e) => {
                    github["repos"] = json!([]);
                    github["error"] = json!(e);
                }
            }
        }
        json!({ "local": local, "github": github, "workspaceDefault": ws_default, "jobs": jobs })
    })
    .await?;
    Ok(Json(v))
}

// ── Clone / fetch jobs ─────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneReq {
    repo: String,
    /// Clone the signed-in account's fork of `repo` instead, with `repo` as
    /// its `upstream`. Never creates a fork: no fork → 404.
    #[serde(default)]
    via_fork: bool,
}

/// How a clone/fetch job sets up `upstream` for a fork.
#[derive(Debug, Clone)]
pub struct ForkPlan {
    pub upstream: String,
    pub url: String,
    pub base: String,
}

/// The signed-in account's fork of `owner/name`, if it has one (same name
/// under the login; GitHub must report it as a fork of exactly that repo).
fn find_own_fork(gh: &dyn GitHub, login: &str, owner: &str, name: &str) -> Result<Option<RepoMeta>, String> {
    if login.eq_ignore_ascii_case(owner) {
        return Ok(None);
    }
    Ok(gh.repo_meta(login, name)?.filter(|m| {
        m.parent.as_ref().is_some_and(|(po, pn, _)| po.eq_ignore_ascii_case(owner) && pn.eq_ignore_ascii_case(name))
    }))
}

/// The upstream setup for cloning `owner/name`: its parent when GitHub says
/// it's a fork (nothing when signed out or GitHub can't tell).
fn fork_plan(gh: &dyn GitHub, owner: &str, name: &str) -> Option<ForkPlan> {
    if !gh.status().available {
        return None;
    }
    let (po, pn, base) = gh.repo_meta(owner, name).ok()??.parent?;
    Some(ForkPlan { upstream: format!("{po}/{pn}"), url: gh.clone_url(&po, &pn), base })
}

#[derive(Deserialize)]
pub struct ForkQuery {
    repo: String,
}

/// `GET /v1/repos/fork?repo=owner/name`: whether the signed-in account has a
/// fork of `owner/name` (so the phone can offer "clone via my fork").
pub async fn fork_of(State(s): S, Query(q): Query<ForkQuery>) -> ApiResult<Json<Value>> {
    let (owner, name) = repos::parse_repo_spec(&q.repo).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let gh = s.repos.github();
    let repos_dir = repos::repos_dir(&s.cfg.data_dir);
    let v = blocking(move || -> ApiResult<Value> {
        let st = gh.status();
        let Some(login) = st.login.filter(|_| st.available) else {
            return Err(err(StatusCode::PRECONDITION_FAILED, "GitHub isn't signed in on this host: run gh auth login on the host"));
        };
        let fork = find_own_fork(gh.as_ref(), &login, &owner, &name).map_err(|e| err(StatusCode::BAD_GATEWAY, e))?;
        let fork = fork.map(|m| {
            let checkout = repos::checkout_name(&m.owner, &m.name);
            let local = repos_dir.join(&checkout).join(".git").exists();
            json!({ "fullName": format!("{}/{}", m.owner, m.name), "checkout": checkout, "cloned": local })
        });
        Ok(json!({ "repo": format!("{owner}/{name}"), "login": login, "fork": fork }))
    })
    .await??;
    Ok(Json(v))
}

/// git's progress line → (phase, percent): `Receiving objects:  45% (…)`.
pub fn parse_progress(line: &str) -> Option<(String, Option<u8>)> {
    let line = line.trim().trim_start_matches("remote:").trim();
    let (phase, rest) = line.split_once(':')?;
    if !phase.contains(' ') || phase.len() > 40 || !phase.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') {
        return None;
    }
    let pct = rest.split_whitespace().next().and_then(|w| w.strip_suffix('%')).and_then(|n| n.parse::<u8>().ok());
    Some((phase.to_string(), pct.filter(|p| *p <= 100)))
}

fn dir_size(p: &FsPath) -> u64 {
    let mut total = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

pub async fn clone_repo(State(s): S, Json(req): Json<CloneReq>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    let (mut owner, mut name) = repos::parse_repo_spec(&req.repo).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let gh = s.repos.github();
    // "Clone the parent via my fork": swap in the account's fork, or report
    // that there isn't one (this host never creates forks).
    if req.via_fork {
        let (g, o, n) = (gh.clone(), owner.clone(), name.clone());
        let found = blocking(move || -> ApiResult<Option<RepoMeta>> {
            let st = g.status();
            let Some(login) = st.login.filter(|_| st.available) else {
                return Err(err(StatusCode::PRECONDITION_FAILED, "cloning via your fork needs GitHub: run gh auth login on the host"));
            };
            find_own_fork(g.as_ref(), &login, &o, &n).map_err(|e| err(StatusCode::BAD_GATEWAY, e))
        })
        .await??;
        let Some(f) = found else {
            return Err(ApiError::not_found(&format!(
                "a fork of {owner}/{name} under your account (this host doesn't create forks; fork it on GitHub, or clone {owner}/{name} directly)"
            )));
        };
        (owner, name) = (f.owner, f.name);
    }
    let full = format!("{owner}/{name}");
    let checkout = repos::checkout_name(&owner, &name);
    let repos_dir = repos::repos_dir(&s.cfg.data_dir);
    let dest = repos_dir.join(&checkout);

    // Idempotent: the same repo already in flight returns that job.
    {
        let jobs = s.repos.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(j) = jobs.iter().find(|j| j.state == "running" && j.checkout == checkout) {
            return Ok(Json(json!({ "job": j.dto() })));
        }
        if jobs.iter().filter(|j| j.state == "running").count() >= MAX_JOBS {
            return Err(err(StatusCode::TOO_MANY_REQUESTS, "two clones are already running; try again when one finishes"));
        }
    }
    let action = if dest.exists() {
        let d = dest.clone();
        let origin = blocking(move || repos::git_out(&d, &["remote", "get-url", "origin"])).await?;
        let same = origin
            .as_deref()
            .and_then(repos::parse_remote)
            .is_some_and(|r| r.owner.eq_ignore_ascii_case(&owner) && r.name.eq_ignore_ascii_case(&name));
        if !same {
            return Err(err(StatusCode::CONFLICT, format!("{checkout} already exists on this host and isn't a clone of {full}")));
        }
        "fetch"
    } else {
        "clone"
    };
    let guard_name = checkout.clone();
    if !s.repos.busy.lock().unwrap_or_else(|e| e.into_inner()).insert(guard_name.clone()) {
        return Err(err(StatusCode::CONFLICT, format!("{checkout} is busy with another git operation")));
    }
    let job = CloneJob {
        id: Uuid::new_v4(),
        repo: full.clone(),
        checkout: checkout.clone(),
        action,
        state: "running",
        phase: if action == "clone" { "Starting clone".into() } else { "Fetching".into() },
        percent: None,
        error: None,
        started_at: Timestamp::now(),
        finished_at: None,
    };
    {
        let mut jobs = s.repos.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.push(job.clone());
        let done = jobs.iter().filter(|j| j.state != "running").count();
        if done > KEEP_JOBS {
            let mut drop_n = done - KEEP_JOBS;
            jobs.retain(|j| {
                if drop_n > 0 && j.state != "running" {
                    drop_n -= 1;
                    false
                } else {
                    true
                }
            });
        }
    }
    s.hub.emit("repo.clone", json!({ "job": job.dto() }));
    let shared: Arc<Shared> = s.clone();
    let id = job.id;
    let url = gh.clone_url(&owner, &name);
    std::thread::spawn(move || {
        let plan = fork_plan(gh.as_ref(), &owner, &name);
        let result = run_clone_job(&shared, id, action, &url, &repos_dir, &dest, gh.git_auth_args())
            .and_then(|()| setup_fork(&shared, id, &dest, plan.as_ref(), action == "clone", &gh.git_auth_args()));
        shared.repos.busy.lock().unwrap_or_else(|e| e.into_inner()).remove(&guard_name);
        let finished = shared.repos.update_job(id, |j| {
            j.finished_at = Some(Timestamp::now());
            match result {
                Ok(()) => {
                    j.state = "done";
                    j.percent = Some(100);
                    j.phase = "Done".into();
                }
                Err(e) => {
                    j.state = "error";
                    j.error = Some(e);
                }
            }
        });
        if let Some(j) = finished {
            shared.hub.emit("repo.clone", json!({ "job": j.dto() }));
        }
    });
    Ok(Json(json!({ "job": job.dto() })))
}

fn run_clone_job(
    s: &Shared,
    id: Uuid,
    action: &str,
    url: &str,
    repos_dir: &FsPath,
    dest: &FsPath,
    auth: Vec<String>,
) -> Result<(), String> {
    std::fs::create_dir_all(repos_dir).map_err(|e| format!("creating the repos dir: {e}"))?;
    // Clone into a hidden temp dir and rename on success, so a half-done
    // clone never shows up (or gets bound) as a checkout.
    let tmp = repos_dir.join(format!(".clone-{id}"));
    let mut c = quiet("git");
    let watch_dir: PathBuf = if action == "clone" {
        c.args(&auth).args(["clone", "--progress", "--no-recurse-submodules", "--", url]).arg(&tmp);
        tmp.clone()
    } else {
        c.arg("-C").arg(dest).args(&auth).args(["fetch", "--prune", "--progress", "origin"]);
        dest.to_path_buf()
    };
    let limit = max_clone_bytes();
    let start_size = if action == "fetch" { dir_size(&watch_dir) } else { 0 };
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let out = run_watched(
        c,
        clone_timeout(),
        |line| {
            if let Some((phase, pct)) = parse_progress(line) {
                let j = s.repos.update_job(id, |j| {
                    j.phase = phase;
                    j.percent = pct;
                });
                if last_emit.elapsed() > Duration::from_millis(400) {
                    last_emit = Instant::now();
                    if let Some(j) = j {
                        s.hub.emit("repo.clone", json!({ "job": j.dto() }));
                    }
                }
            }
        },
        || {
            (dir_size(&watch_dir).saturating_sub(start_size) > limit)
                .then(|| format!("stopped: the repository is larger than {} MB (HIVE_REPOS_MAX_CLONE_MB)", limit / 1024 / 1024))
        },
    );
    let fail = |e: String| {
        if action == "clone" {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        Err(e)
    };
    match out {
        Err(e) => return fail(e),
        Ok(o) if !o.ok => {
            let msg = last_lines(&o.stderr);
            let hint = if msg.contains("could not read Username") || msg.contains("Authentication failed") || msg.contains("not found") {
                " (a private repository needs gh auth login on the host)"
            } else {
                ""
            };
            return fail(format!("git {action} failed: {msg}{hint}"));
        }
        Ok(_) => {}
    }
    if action == "clone" {
        if dest.exists() {
            return fail(format!("{} appeared while cloning", dest.file_name().unwrap_or_default().to_string_lossy()));
        }
        std::fs::rename(&tmp, dest).map_err(|e| format!("moving the clone into place: {e}"))?;
    }
    Ok(())
}

/// After a clone or fetch: make a fork checkout fork-aware. Adds `upstream`
/// (fetch-only: its push URL is a dummy) when GitHub says the repo is a fork
/// and it isn't there yet, fetches it, records upstream's default branch, and
/// on a fresh clone points the local base branch at upstream's.
fn setup_fork(s: &Shared, id: Uuid, dest: &FsPath, plan: Option<&ForkPlan>, fresh: bool, auth: &[String]) -> Result<(), String> {
    let has_upstream = repos::git_out(dest, &["remote", "get-url", UPSTREAM_REMOTE]).is_some();
    if !has_upstream {
        let Some(p) = plan else { return Ok(()) };
        git(dest, &[], &["remote", "add", UPSTREAM_REMOTE, &p.url], GIT_TIMEOUT).map_err(|e| format!("adding upstream: {e}"))?;
    }
    // Fetch-only, whatever was there before.
    if !has_upstream || repos::upstream_push_enabled(dest) {
        git(dest, &[], &["remote", "set-url", "--push", UPSTREAM_REMOTE, "DISABLED-push-to-fork-only"], GIT_TIMEOUT)
            .map_err(|e| format!("disabling pushes to upstream: {e}"))?;
    }
    if let Some(j) = s.repos.update_job(id, |j| {
        j.phase = format!("Fetching {}", plan.map(|p| p.upstream.as_str()).unwrap_or(UPSTREAM_REMOTE));
        j.percent = None;
    }) {
        s.hub.emit("repo.clone", json!({ "job": j.dto() }));
    }
    git(dest, auth, &["fetch", "--prune", "--no-tags", UPSTREAM_REMOTE], clone_timeout()).map_err(|e| format!("git fetch upstream: {e}"))?;
    let base = plan
        .map(|p| p.base.clone())
        .filter(|b| !b.is_empty() && repos::ref_exists(dest, &format!("refs/remotes/{UPSTREAM_REMOTE}/{b}")));
    let base = match base {
        Some(b) => {
            let _ = git(dest, &[], &["remote", "set-head", UPSTREAM_REMOTE, &b], GIT_TIMEOUT);
            b
        }
        None => {
            let _ = git(dest, &[], &["remote", "set-head", UPSTREAM_REMOTE, "--auto"], GIT_TIMEOUT);
            match repos::fork_layout(dest) {
                Some(f) => f.base_branch,
                None => return Ok(()),
            }
        }
    };
    let up = format!("{UPSTREAM_REMOTE}/{base}");
    if fresh {
        // Nothing local yet: the base branch simply follows upstream.
        git(dest, &[], &["checkout", "-q", "-B", &base, "--track", &up], GIT_TIMEOUT).map_err(|e| format!("tracking {up}: {e}"))?;
    } else if repos::ref_exists(dest, &format!("refs/heads/{base}")) {
        let _ = git(dest, &[], &["branch", "--set-upstream-to", &up, &base], GIT_TIMEOUT);
    }
    repos::ensure_chat_base(dest);
    Ok(())
}

pub async fn clone_job(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let id = Uuid::parse_str(id.trim()).map_err(|_| ApiError::not_found("job"))?;
    let j = s.repos.job(id).ok_or_else(|| ApiError::not_found("job"))?;
    Ok(Json(json!({ "job": j.dto() })))
}

// ── Chat binding ───────────────────────────────────────────────────────────

pub async fn get_chat_repo(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    let dir = s.cfg.data_dir.clone();
    Ok(Json(blocking(move || chat_binding(&dir, chat.id).1).await?))
}

#[derive(Deserialize, Default)]
pub struct SetRepo {
    #[serde(default)]
    repo: Option<String>,
}

pub async fn put_chat_repo(State(s): S, Path(id): Path<String>, body: Bytes) -> ApiResult<Json<Value>> {
    let req: SetRepo = if body.iter().all(u8::is_ascii_whitespace) {
        SetRepo::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))?
    };
    let chat = load_chat(&s, &id)?;
    ensure_can_write(&s, Some(&chat))?;
    let repos_dir = repos::repos_dir(&s.cfg.data_dir);
    // `null` / "" drops the chat's own binding (back to the workspace default).
    let target = match req.repo.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        None => None,
        Some(r) => {
            // `owner/name` names the checkout it was cloned into.
            let name = if r.contains('/') {
                let (o, n) = repos::parse_repo_spec(r).map_err(|e| ApiError::bad_request(e.to_string()))?;
                repos::checkout_name(&o, &n)
            } else {
                r.to_string()
            };
            if !repos::valid_checkout_dir_name(&name) {
                return Err(ApiError::bad_request("repo must be the name of a checkout on this host"));
            }
            let d = repos_dir.join(&name);
            if !blocking(move || hive_runtime::git_worktree::is_git_repo(&d)).await? {
                return Err(ApiError::not_found(&format!("checkout '{name}' (clone it first)")));
            }
            Some(name)
        }
    };
    let changed = {
        let _g = s.repos.bindings.lock().unwrap_or_else(|e| e.into_inner());
        let mut b = RepoBindings::load(&s.cfg.data_dir);
        let key = chat.id.to_string();
        let before = b.chats.get(&key).cloned();
        match &target {
            Some(t) => b.chats.insert(key, t.clone()),
            None => b.chats.remove(&key),
        };
        let changed = before != target;
        if changed {
            b.save(&s.cfg.data_dir)?;
        }
        changed
    };
    let dir = s.cfg.data_dir.clone();
    let cid = chat.id;
    let v = blocking(move || chat_binding(&dir, cid).1).await?;
    if changed {
        let what = v["repo"]["displayName"].as_str().map(str::to_string);
        let note = match (&target, what) {
            (_, Some(name)) => format!(
                "This chat now works in {name} on {} (branch {}).",
                s.opts.label,
                repos::chat_branch(chat.id)
            ),
            (_, None) => format!("This chat no longer has a repository on {}.", s.opts.label),
        };
        s.lock().post_system_note(chat.id, chat.workspace_id, note)?;
        s.after_write();
    }
    Ok(Json(v))
}

/// The bound checkout of `chat`, or 409.
fn bound_dir(s: &Shared, chat: Uuid) -> ApiResult<(PathBuf, String)> {
    let b = RepoBindings::load(&s.cfg.data_dir);
    let (raw, _) = b.raw_for(chat).ok_or_else(|| err(StatusCode::CONFLICT, "this chat has no repository on this host"))?;
    let dir = repos::resolve_path(&repos::repos_dir(&s.cfg.data_dir), raw);
    if !hive_runtime::git_worktree::is_git_repo(&dir) {
        return Err(err(StatusCode::CONFLICT, "this chat's checkout is missing on this host"));
    }
    Ok((dir, binding_name(raw).0))
}

pub async fn pull_chat_repo(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    ensure_can_write(&s, Some(&chat))?;
    let (dir, name) = bound_dir(&s, chat.id)?;
    let _busy = claim(&s.repos, &name)?;
    let auth = s.repos.github().git_auth_args();
    let d = dir.clone();
    let res = blocking(move || -> Result<(String, String), (StatusCode, String)> {
        let info = repos::inspect(&d);
        let conflict = |m: String| (StatusCode::CONFLICT, m);
        if info.dirty {
            return Err(conflict("the checkout has uncommitted changes; pull only fast-forwards a clean checkout".into()));
        }
        let branch = info.branch.ok_or_else(|| conflict("the checkout isn't on a branch".into()))?;
        let before = info.sha.unwrap_or_default();
        git(&d, &auth, &["fetch", "--prune", "origin"], GIT_TIMEOUT).map_err(|e| (StatusCode::BAD_GATEWAY, format!("git fetch: {e}")))?;
        if let Some(f) = info.fork {
            // A fork: fast-forward the base from the upstream project.
            git(&d, &auth, &["fetch", "--prune", "--no-tags", UPSTREAM_REMOTE], GIT_TIMEOUT)
                .map_err(|e| (StatusCode::BAD_GATEWAY, format!("git fetch upstream: {e}")))?;
            let up = f.base_ref();
            let local_base = format!("refs/heads/{}", f.base_branch);
            let before = repos::git_out(&d, &["rev-parse", "--short", &local_base]).unwrap_or_default();
            if !repos::ref_exists(&d, &up) {
                return Err(conflict(format!("upstream has no branch {}", f.base_branch)));
            }
            let diverged = || conflict(format!("can't fast-forward {}: it has diverged from {}", f.base_branch, f.upstream.full_name()));
            if branch == f.base_branch {
                git(&d, &[], &["merge", "--ff-only", &up], GIT_TIMEOUT).map_err(|_| diverged())?;
            } else {
                // Not checked out: a fast-forward-only ref update (no `+`).
                git(&d, &[], &["fetch", ".", &format!("{up}:refs/heads/{}", f.base_branch)], GIT_TIMEOUT).map_err(|_| diverged())?;
            }
            repos::ensure_chat_base(&d);
            let after = repos::git_out(&d, &["rev-parse", "--short", &local_base]).unwrap_or_default();
            return Ok((before, after));
        }
        let upstream = format!("refs/remotes/origin/{branch}");
        if repos::git_out(&d, &["rev-parse", "--verify", "--quiet", &upstream]).is_none() {
            return Err(conflict(format!("origin has no branch {branch}")));
        }
        git(&d, &[], &["merge", "--ff-only", &upstream], GIT_TIMEOUT)
            .map_err(|_| conflict(format!("can't fast-forward {branch}: it has diverged from origin")))?;
        let after = repos::git_out(&d, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        Ok((before, after))
    })
    .await?
    .map_err(|(c, m)| err(c, m))?;
    let cid = chat.id;
    let data = s.cfg.data_dir.clone();
    let mut v = blocking(move || chat_binding(&data, cid).1).await?;
    v["updated"] = json!(res.0 != res.1);
    v["before"] = json!(res.0);
    v["after"] = json!(res.1);
    Ok(Json(v))
}

// ── Pull request ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PrReq {
    title: String,
    #[serde(default)]
    body: Option<String>,
    /// Default: a draft when the PR opens upstream from a fork, else not.
    #[serde(default)]
    draft: Option<bool>,
}

pub async fn open_pr(State(s): S, Path(id): Path<String>, Json(req): Json<PrReq>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    ensure_can_write(&s, Some(&chat))?;
    let title = req.title.trim().to_string();
    if title.is_empty() || title.chars().count() > 256 {
        return Err(ApiError::bad_request("a pull request needs a title (up to 256 characters)"));
    }
    let body = req.body.clone().unwrap_or_default();
    if body.len() > 60_000 {
        return Err(ApiError::bad_request("the pull request body is too long"));
    }
    let (dir, name) = bound_dir(&s, chat.id)?;
    let gh = s.repos.github();
    let _busy = claim(&s.repos, &name)?;
    let branch = repos::chat_branch(chat.id);
    let shared: Arc<Shared> = s.clone();
    let (chat_id, chat_title) = (chat.id, chat.title.clone());
    let res = blocking(move || -> Result<PrDone, (StatusCode, String)> {
        let conflict = |m: String| (StatusCode::CONFLICT, m);
        let origin = repos::git_out(&dir, &["remote", "get-url", "origin"]).unwrap_or_default();
        let remote = gh
            .repo_of_remote(&origin)
            .ok_or_else(|| conflict("this checkout's origin isn't a GitHub repository".into()))?;
        let st = gh.status();
        if !st.available {
            return Err((
                StatusCode::PRECONDITION_FAILED,
                format!("GitHub isn't signed in on this host: {}", st.reason.unwrap_or_else(|| "run gh auth login on the host".into())),
            ));
        }
        if !repos::ref_exists(&dir, &format!("refs/heads/{branch}")) {
            return Err(conflict(format!(
                "this chat has no commits on {branch} yet: approve a proposal first (the host commits approved changes \
                 when HIVE_SERVE_APPLY_ON_APPROVE=1)"
            )));
        }
        let info = repos::inspect(&dir);
        let auth = gh.git_auth_args();
        let Some(fork) = info.fork else {
            // A fork without an `upstream` remote would open the PR against the fork itself.
            if let Ok(Some(RepoMeta { parent: Some((po, pn, _)), .. })) = gh.repo_meta(&remote.owner, &remote.name) {
                return Err(conflict(format!(
                    "{} is a fork of {po}/{pn} but this checkout has no upstream remote: fetch it from the repo \
                     picker (that adds upstream), then open the PR",
                    remote.full_name()
                )));
            }
            repos::check_push_target(&dir, "origin", None).map_err(conflict)?;
            let base = info.default_branch.ok_or_else(|| conflict("can't tell the default branch".into()))?;
            git(&dir, &auth, &["fetch", "origin", &base], GIT_TIMEOUT).map_err(|e| (StatusCode::BAD_GATEWAY, format!("git fetch: {e}")))?;
            let ahead = repos::git_out(&dir, &["rev-list", "--count", &format!("refs/remotes/origin/{base}..refs/heads/{branch}")])
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0);
            if ahead == 0 {
                return Err(conflict(format!("{branch} has nothing that {base} doesn't already have")));
            }
            // Never a force push: no `+`, no --force; a diverged remote branch is refused.
            let spec = format!("refs/heads/{branch}:refs/heads/{branch}");
            git(&dir, &auth, &["push", "origin", &spec], GIT_TIMEOUT).map_err(|e| conflict(format!("git push: {e}")))?;
            let pr = PrRequest { repo: remote.full_name(), head: branch.clone(), base: base.clone(), title, body, draft: req.draft.unwrap_or(false) };
            let (url, existing) = gh.create_pr(&dir, &pr).map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
            return Ok(PrDone { url, existing, base, pushed: branch, repo: pr.repo, draft: pr.draft });
        };
        // A fork: push to origin (never upstream) under a readable branch,
        // then open the PR on the upstream project.
        let target = repos::check_push_target(&dir, "origin", Some(&fork.upstream)).map_err(conflict)?;
        if !repos::same_repo(&target, &remote) {
            return Err(conflict(format!("origin pushes to {}, not the fork {}: fix the remote first", target.full_name(), remote.full_name())));
        }
        let base = fork.base_branch.clone();
        git(&dir, &auth, &["fetch", "--no-tags", UPSTREAM_REMOTE, &base], GIT_TIMEOUT)
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("git fetch upstream: {e}")))?;
        let ahead = repos::git_out(&dir, &["rev-list", "--count", &format!("{}..refs/heads/{branch}", fork.base_ref())])
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        if ahead == 0 {
            return Err(conflict(format!("{branch} has nothing that {} {base} doesn't already have", fork.upstream.full_name())));
        }
        let pushed = {
            let _g = shared.repos.bindings.lock().unwrap_or_else(|e| e.into_inner());
            let mut map = PrBranches::load(&shared.cfg.data_dir);
            let b = map.get_or_assign(&name, chat_id, &chat_title);
            map.save(&shared.cfg.data_dir).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            b
        };
        // Never a force push: a branch that moved on the fork is refused.
        let spec = format!("refs/heads/{branch}:refs/heads/{pushed}");
        git(&dir, &auth, &["push", "origin", &spec], GIT_TIMEOUT).map_err(|e| conflict(format!("git push to your fork: {e}")))?;
        let pr = PrRequest {
            repo: fork.upstream.full_name(),
            head: format!("{}:{pushed}", remote.owner),
            base: base.clone(),
            title,
            body,
            draft: req.draft.unwrap_or(true),
        };
        let (url, existing) = gh.create_pr(&dir, &pr).map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        Ok(PrDone { url, existing, base, pushed, repo: pr.repo, draft: pr.draft })
    })
    .await?
    .map_err(|(c, m)| err(c, m))?;
    let PrDone { url, existing, base, pushed, repo, draft } = res;
    let note = if existing {
        format!("Pull request (already open, branch updated): {url}")
    } else if draft {
        format!("Opened pull request: {url} (draft, on {repo})")
    } else {
        format!("Opened pull request: {url}")
    };
    s.lock().post_system_note(chat.id, chat.workspace_id, note)?;
    s.after_write();
    Ok(Json(json!({
        "url": url,
        "existing": existing,
        "branch": repos::chat_branch(chat.id),
        "pushedBranch": pushed,
        "repo": repo,
        "base": base,
        "draft": draft,
    })))
}

struct PrDone {
    url: String,
    existing: bool,
    base: String,
    /// The branch name on the remote the PR's head was pushed as.
    pushed: String,
    /// The repository the PR is on.
    repo: String,
    draft: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_parse() {
        assert_eq!(parse_progress("Receiving objects:  45% (450/1000), 1.2 MiB | 2 MiB/s"), Some(("Receiving objects".into(), Some(45))));
        assert_eq!(parse_progress("remote: Counting objects: 100% (5/5), done."), Some(("Counting objects".into(), Some(100))));
        assert_eq!(parse_progress("Cloning into '/x/.clone-1'..."), None);
        assert_eq!(parse_progress("fatal: repository not found"), None);
    }

    #[test]
    fn run_times_out_and_kills() {
        let mut c = Command::new("sleep");
        c.arg("5");
        let t = Instant::now();
        let e = run(c, Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn check_can_stop_a_command() {
        let mut c = Command::new("sleep");
        c.arg("5");
        let e = run_watched(c, Duration::from_secs(10), |_| {}, || Some("too big".into())).unwrap_err();
        assert_eq!(e, "too big");
    }

    #[test]
    fn absolute_bindings_never_expose_paths() {
        assert_eq!(binding_name("/home/x/secret/repo"), ("repo".into(), true));
        assert_eq!(binding_name("sandbox"), ("sandbox".into(), false));
    }
}

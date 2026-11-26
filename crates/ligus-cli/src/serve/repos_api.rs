//! Repos on the home host, from the phone: list the box's checkouts and the
//! repos its code-host connections can see (GitHub, GitLab, Forgejo, Gitea,
//! Codeberg), clone one on demand, switch which checkout a chat works in,
//! pull the base branch, and open a pull or merge request from the chat's
//! branch.
//!
//! Everything here acts on this box's own device-local state
//! (`<LIGUS_DATA_DIR>/repo-bindings.toml` and the checkouts under
//! `LIGUS_REPOS_DIR`, see `crate::repos`); nothing is synced. Code-host
//! access goes through the box's connections (`<data dir>/forges.toml`, see
//! `ligus_forge::conn`): the box's own `gh` login for GitHub by default, or
//! tokens in the box's OS secret store. Tests swap in fake forges.
//!
//! Routes (contract in `docs/ios-app.md`):
//! - `GET  /v1/repos?q=&owner=&page=&perPage=[&forge=id]`
//! - `GET  /v1/repos/fork?repo=owner/name[&forge=id]` → the signed-in account's fork of it
//! - `POST /v1/repos/clone {repo, viaFork?, forge?}` → a job; `GET /v1/repos/clone/:job`
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use ligus_core::{ChatSession, Timestamp, WorkspaceRole};
use ligus_forge::{ChangeRequestDraft, ChangeRequestOutcome, Connection, Connections, Forge, ForgeId, ForgeKind, RepoQuery, RepoRef, Transport};
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
    env_secs("LIGUS_REPOS_CLONE_TIMEOUT_SECS", 900)
}

fn max_clone_bytes() -> u64 {
    crate::env_opt("LIGUS_REPOS_MAX_CLONE_MB").and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(2048) * 1024 * 1024
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
    // git: Ligus's own hardened git (no inherited repo redirects); either
    // way none of Ligus's own secrets reach the child or its helpers.
    let mut c = if program == "git" { ligus_core::process_util::git() } else { ligus_core::process_util::command(program) };
    ligus_core::process_util::scrub_ligus_secrets(&mut c);
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
    // A branch it creates or moves (tracking, fast-forwarding the base)
    // waits for the repo's worktree and branch mutations, like a turn's cut.
    let _lock = ligus_runtime::git_worktree::lock_if_mutating(dir, args);
    let mut c = quiet("git");
    ligus_core::process_util::harden_git(&mut c);
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

// ── Code hosts (the box's forge connections) ───────────────────────────────

/// Whether a connection can be used, and as whom.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhStatus {
    pub available: bool,
    pub login: Option<String>,
    pub reason: Option<String>,
}

/// One code-host connection on this box and its forge.
#[derive(Clone)]
pub struct ForgeHandle {
    pub conn: Connection,
    pub forge: Arc<dyn Forge>,
}

impl ForgeHandle {
    pub fn id(&self) -> &ForgeId {
        &self.conn.id
    }
    pub fn is_github_com(&self) -> bool {
        self.conn.kind == ForgeKind::GitHub && self.conn.host() == "github.com"
    }
}

/// The connections of `data_dir`, built with the box's secret store and
/// this binary as the git credential helper.
pub fn load_forges(data_dir: &FsPath, offline: bool) -> Vec<ForgeHandle> {
    let store = crate::forge_cmd::token_store(data_dir);
    let helper = crate::forge_cmd::helper_program();
    Connections::load(data_dir)
        .forges
        .into_iter()
        .map(|conn| {
            // Offline (tests): never the host's gh login.
            let creds: Arc<dyn ligus_forge::CredentialSource> = if offline && conn.auth == ligus_forge::AuthSource::Gh {
                Arc::new(ligus_forge::tokens::Anonymous)
            } else {
                conn.credentials(store.clone(), helper.as_deref(), Some(data_dir))
            };
            let forge = conn.build(creds);
            ForgeHandle { conn, forge }
        })
        .collect()
}

/// Why an offline box (the test build) reports every connection signed out.
pub const OFFLINE: &str = "offline: this build never contacts code hosts";

/// A connection's status: signed in as whom, or why not (cached a minute).
async fn forge_status(st: &RepoState, h: &ForgeHandle) -> GhStatus {
    if let Some((at, s)) = st.status.lock().unwrap_or_else(|e| e.into_inner()).get(h.id()) {
        if at.elapsed() < Duration::from_secs(60) {
            return s.clone();
        }
    }
    if st.offline.load(Ordering::SeqCst) && !st.pinned.load(Ordering::SeqCst) {
        return GhStatus { available: false, login: None, reason: Some(OFFLINE.into()) };
    }
    let s = match tokio::time::timeout(GH_TIMEOUT, h.forge.whoami()).await {
        Ok(Ok(Some(u))) => GhStatus { available: true, login: Some(u.login), reason: None },
        Ok(Ok(None)) => GhStatus { available: false, login: None, reason: h.forge.signed_out_reason() },
        // Never what the host answered: its URL may not be a code host.
        Ok(Err(e)) => GhStatus { available: false, login: None, reason: Some(format!("{} didn't answer with this box's login: {}", h.conn.display(), e.summary())) },
        Err(_) => GhStatus { available: false, login: None, reason: Some(format!("{} didn't answer in time", h.conn.display())) },
    };
    st.status.lock().unwrap_or_else(|e| e.into_inner()).insert(h.id().clone(), (Instant::now(), s.clone()));
    s
}

/// [`forge_status`] for the forges API.
pub(super) async fn forge_status_pub(st: &RepoState, h: &ForgeHandle) -> GhStatus {
    forge_status(st, h).await
}

/// One forge connection as the phone sees it (never a token).
pub fn forge_dto(h: &ForgeHandle, status: Option<&GhStatus>) -> Value {
    let caps = h.forge.capabilities();
    let mut v = json!({
        "id": h.conn.id.as_str(),
        "kind": h.conn.kind.as_str(),
        "flavor": h.conn.flavor,
        "displayName": h.conn.display(),
        "baseUrl": h.conn.base_url,
        "authSource": h.conn.auth.as_str(),
        "hasWrite": h.forge.has_write(),
        "changeRequestKind": h.conn.kind.change_request_kind().as_str(),
        "openLabel": h.conn.kind.open_label(),
        "deviceFlow": caps.device_flow && ligus_forge::device::endpoints(&h.conn).is_ok(),
        "capabilities": caps,
        "holder": "this box",
        "internal": h.conn.internal,
    });
    if let Some(s) = status {
        v["available"] = json!(s.available);
        if let Some(l) = &s.login {
            v["user"] = json!(l);
        }
        if let Some(r) = &s.reason {
            v["reason"] = json!(r);
        }
    }
    v
}

// ── Shared state: forges, clone jobs, per-checkout locks ───────────────────

/// A device sign-in the box is polling for.
#[derive(Debug, Clone)]
pub struct DeviceSession {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at: Instant,
    /// `pending`, `done`, `denied`, `expired` or `error`.
    pub state: &'static str,
    pub error: Option<String>,
    /// Which sign-in this is: a poll saves its tokens only while its own
    /// session is still the pending one for the connection.
    pub nonce: u64,
}

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
    /// The box's connections (`None` = load from `forges.toml` on next use).
    forges: RwLock<Option<Vec<ForgeHandle>>>,
    /// Set by tests: keep the injected forges, never reload from disk.
    pinned: AtomicBool,
    /// Never contact a code host, and never ask the host's `gh` (on in
    /// test builds, so no test reaches a real forge or the developer's own
    /// login; injected fakes still answer).
    pub(super) offline: AtomicBool,
    status: Mutex<HashMap<ForgeId, (Instant, GhStatus)>>,
    pub(super) device: Mutex<HashMap<ForgeId, DeviceSession>>,
    jobs: Mutex<Vec<CloneJob>>,
    /// Checkout names with a git operation in flight.
    busy: Mutex<HashSet<String>>,
    /// Serializes read-modify-write of the bindings file (and forges.toml).
    pub(super) bindings: Mutex<()>,
}

impl Default for RepoState {
    fn default() -> Self {
        Self {
            forges: RwLock::new(None),
            pinned: AtomicBool::new(false),
            offline: AtomicBool::new(cfg!(test)),
            status: Mutex::new(HashMap::new()),
            device: Mutex::new(HashMap::new()),
            jobs: Mutex::new(Vec::new()),
            busy: Mutex::new(HashSet::new()),
            bindings: Mutex::new(()),
        }
    }
}

impl RepoState {
    /// The box's connections.
    pub fn forges(&self, data_dir: &FsPath) -> Vec<ForgeHandle> {
        if let Some(f) = self.forges.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            return f.clone();
        }
        let loaded = load_forges(data_dir, self.offline.load(Ordering::SeqCst));
        *self.forges.write().unwrap_or_else(|e| e.into_inner()) = Some(loaded.clone());
        loaded
    }

    /// Reload connections from disk on next use (after an edit).
    pub fn invalidate(&self) {
        if !self.pinned.load(Ordering::SeqCst) {
            *self.forges.write().unwrap_or_else(|e| e.into_inner()) = None;
        }
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// The connection with this id (`github.com` is the GitHub alias).
    /// Whether code hosts may be contacted (false in test builds).
    pub fn online(&self) -> bool {
        !self.offline.load(Ordering::SeqCst) || self.pinned.load(Ordering::SeqCst)
    }

    pub fn forge(&self, data_dir: &FsPath, id: &ForgeId) -> Option<ForgeHandle> {
        self.forges(data_dir).into_iter().find(|h| h.id() == id)
    }

    /// The GitHub connection the legacy `github` listing key shows.
    pub fn github(&self, data_dir: &FsPath) -> Option<ForgeHandle> {
        let all = self.forges(data_dir);
        all.iter().find(|h| h.is_github_com()).or_else(|| all.iter().find(|h| h.conn.kind == ForgeKind::GitHub)).cloned()
    }

    /// The connection a remote URL is on.
    pub fn forge_of_remote(&self, data_dir: &FsPath, url: &str) -> Option<(ForgeHandle, RepoRef)> {
        self.forges(data_dir).into_iter().find_map(|h| h.forge.repo_of_remote(url).map(|r| (h, r)))
    }

    /// Swap the forges (tests).
    #[cfg(test)]
    pub fn set_forges(&self, forges: Vec<ForgeHandle>) {
        self.pinned.store(true, Ordering::SeqCst);
        *self.forges.write().unwrap_or_else(|e| e.into_inner()) = Some(forges);
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
pub(super) fn ensure_can_write(s: &Shared, chat: Option<&ChatSession>) -> ApiResult<()> {
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

/// The refusal code of [`owner_gate`].
pub const OWNER_REQUIRED: &str = "owner_required";

/// Repos review F12 and residual 2: what spends this box's own forge access
/// or steers its checkouts (a clone by spec or by id, renaming a repo,
/// setting a chat's repo, a rescan, confirming a found checkout) is the box
/// owner's: a phone certified to the account of the person who admitted the
/// box (`ChatService::box_owner_accounts`), through its own pairing, or the
/// `serve.token` bearer on a loopback listener (the operator on the box).
/// Any other paired phone, and the bearer over the network (every phone can
/// fetch it), is refused with [`OWNER_REQUIRED`]. Never a request carried on
/// the log.
pub(super) fn owner_gate(s: &Shared, peer: Option<&super::tunnel::TunnelPeer>) -> ApiResult<()> {
    let ok = match peer {
        // D5: the operator's own token on a loopback listener. The phones'
        // `serve.token` reaches loopback too, through a reverse proxy on
        // this host, so it never counts.
        None => s.opts.listen.ip().is_loopback() && super::api::is_operator(),
        Some(p) => {
            let device = s.tunnel.as_ref().and_then(|t| t.registry.get(&p.pid)).filter(|r| r.is_active()).and_then(|r| r.device_id);
            device.is_some_and(|d| {
                let svc = s.lock();
                svc.box_owner_accounts(s.ws).is_ok_and(|owners| svc.phone_of_accounts(s.ws, d, &owners))
            })
        }
    };
    if ok {
        Ok(())
    } else {
        Err(err(
            StatusCode::FORBIDDEN,
            format!("{OWNER_REQUIRED}: only the phone of the person who admitted this box (certified to their account), or the operator on the box itself, can do this"),
        ))
    }
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
    // `has_root`: a drive-less `/home/x` on Windows is still a full path.
    if p.is_absolute() || p.has_root() {
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
    if !ligus_runtime::git_worktree::is_git_repo(&dir) {
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
    let mut v = chat_binding(&s.cfg.data_dir, chat).1;
    tag_scratch(s, chat, &mut v);
    v
}

/// Mark a chat binding's repo as scratch (`repo.scratch`: dates, never a
/// path) when it is one of this box's live scratch repositories.
pub(super) fn tag_scratch(s: &Shared, chat: Uuid, v: &mut Value) {
    if v["repo"].is_null() {
        return;
    }
    let svc = s.lock();
    if let Some(e) = crate::scratch_cmd::of_chat(&s.cfg.data_dir, &svc, chat) {
        let st = crate::scratch_cmd::settings(&svc, &s.cfg.data_dir);
        v["repo"]["scratch"] = crate::scratch_cmd::dto(&e, &st, ligus_runtime::repos::scratch::now());
        v["repo"]["displayName"] = json!(ligus_runtime::display::shown(&e.name));
        v["repo"]["name"] = json!(e.dir_name);
    }
}

// ── GET /v1/repos ──────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    q: Option<String>,
    owner: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
    /// A connection id (`gitlab:gitlab.example.com`); absent = the legacy
    /// shape, with `github` and the list of `forges`.
    forge: Option<String>,
}

/// An owner, org or group name (GitLab groups nest: `grp/sub`).
fn valid_owner_any(o: &str) -> bool {
    o.len() <= 255 && o.split('/').all(ligus_forge::spec::valid_segment)
}

/// The listing of one connection, in the shape the phone knows from the
/// `github` key.
async fn forge_listing(st: &RepoState, h: &ForgeHandle, query: &RepoQuery, cloned: &HashMap<String, String>, via_fork: &HashMap<String, String>) -> Value {
    let status = forge_status(st, h).await;
    let mut out = json!({ "available": status.available });
    if let Some(r) = &status.reason {
        out["reason"] = json!(r);
    }
    let (true, Some(login)) = (status.available, status.login.as_deref()) else { return out };
    out["login"] = json!(login);
    out["owners"] = match h.forge.owners(login).await {
        Ok(o) => json!(o),
        Err(_) => json!([login]),
    };
    match h.forge.list_repos(query, login).await {
        Ok(list) => {
            let has_more = list.len() as u32 >= query.per_page;
            out["repos"] = json!(list
                .iter()
                .map(|r| {
                    let full = r.repo.full_name();
                    let key = full.to_ascii_lowercase();
                    let mut v = json!({
                        "fullName": full,
                        "owner": r.repo.owner,
                        "name": r.repo.name,
                        "description": r.description,
                        "private": r.private,
                        "defaultBranch": r.default_branch,
                        "updatedAt": r.updated_at,
                        "url": r.web_url,
                        "fork": r.fork,
                    });
                    if let Some(local) = via_fork.get(&key) {
                        v["viaForkCheckout"] = json!(local);
                    }
                    match cloned.get(&key) {
                        Some(local) => {
                            v["cloned"] = json!(true);
                            v["checkout"] = json!(local);
                        }
                        None => v["cloned"] = json!(false),
                    }
                    v
                })
                .collect::<Vec<_>>());
            out["page"] = json!(query.page);
            out["perPage"] = json!(query.per_page);
            out["hasMore"] = json!(has_more);
        }
        Err(e) => {
            out["repos"] = json!([]);
            out["error"] = json!(e.to_string());
        }
    }
    out
}

pub async fn list_repos(State(s): S, peer: Option<axum::Extension<super::tunnel::TunnelPeer>>, Query(lq): Query<ListQuery>) -> ApiResult<Json<Value>> {
    // Lists with the owner's forge sign-in (private repos, any org).
    owner_gate(&s, peer.as_deref())?;
    let q = lq.q.map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
    if q.as_ref().is_some_and(|q| q.chars().count() > 100 || q.chars().any(char::is_control)) {
        return Err(ApiError::bad_request("search text is too long"));
    }
    let owner = lq.owner.map(|o| o.trim().to_string()).filter(|o| !o.is_empty());
    if owner.as_deref().is_some_and(|o| !valid_owner_any(o)) {
        return Err(ApiError::bad_request("owner must be an account, org or group name"));
    }
    let selected = match lq.forge.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        None => None,
        Some(raw) => {
            let id = ForgeId::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))?;
            Some(s.repos.forge(&s.cfg.data_dir, &id).ok_or_else(|| ApiError::not_found("code host connection"))?)
        }
    };
    let query = RepoQuery { q, owner, page: lq.page.unwrap_or(1).clamp(1, 100), per_page: lq.per_page.unwrap_or(30).clamp(1, 100) };
    let data_dir = s.cfg.data_dir.clone();
    let jobs: Vec<Value> = s.repos.running_jobs().iter().map(CloneJob::dto).collect();
    let needle = query.q.as_deref().map(str::to_ascii_lowercase);
    // This box's live scratch repositories, listed first among its
    // checkouts (by folder name, tagged `scratch`), and whether it makes
    // them at all (`scratch` settings; absent from an older host).
    let (scratch_live, scratch_settings) = {
        let svc = s.lock();
        let st = crate::scratch_cmd::settings(&svc, &s.cfg.data_dir);
        let live = ligus_runtime::repos::scratch::live(svc.store(), s.ws);
        let now = ligus_runtime::repos::scratch::now();
        let settings = crate::scratch_cmd::settings_dto(&st, live.len());
        (live.into_iter().map(|e| { let d = crate::scratch_cmd::dto(&e, &st, now); (e, d) }).collect::<Vec<_>>(), settings)
    };
    let (local, cloned, via_fork, ws_default) = blocking(move || {
        let repos_dir = repos::repos_dir(&data_dir);
        let b = RepoBindings::load(&data_dir);
        let ws_default = b.workspace.as_deref().map(|w| binding_name(w).0);
        let mut cloned: HashMap<String, String> = HashMap::new();
        // upstream full name → the local fork checkout of it.
        let mut via_fork: HashMap<String, String> = HashMap::new();
        let mut scratch_rows: Vec<Value> = scratch_live
            .iter()
            .filter(|(e, _)| needle.as_deref().is_none_or(|n| format!("{} {}", e.dir_name, e.name).to_ascii_lowercase().contains(n)))
            .filter(|(e, _)| ligus_runtime::git_worktree::is_git_repo(&e.path))
            .map(|(e, d)| {
                let info = repos::inspect(&e.path);
                let mut v = checkout_dto_at(&e.path, &e.dir_name, &info);
                v["displayName"] = json!(ligus_runtime::display::shown(&e.name));
                v["scratch"] = d.clone();
                v["isWorkspaceDefault"] = json!(false);
                v["boundChats"] = json!(b.chats.values().filter(|c| repos::resolve_path(&repos_dir, c) == e.path).count());
                v
            })
            .collect();
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
        scratch_rows.extend(local);
        let local = scratch_rows;
        (local, cloned, via_fork, ws_default)
    })
    .await?;
    if let Some(h) = selected {
        let forge = forge_listing(&s.repos, &h, &query, &cloned, &via_fork).await;
        let mut v = json!({ "local": local, "forge": forge, "workspaceDefault": ws_default, "jobs": jobs, "scratch": scratch_settings });
        v["forge"]["id"] = json!(h.id().as_str());
        return Ok(Json(v));
    }
    let github = match s.repos.github(&s.cfg.data_dir) {
        Some(h) => forge_listing(&s.repos, &h, &query, &cloned, &via_fork).await,
        None => json!({ "available": false, "reason": "no GitHub connection on this host" }),
    };
    let mut forges = Vec::new();
    for h in s.repos.forges(&s.cfg.data_dir) {
        let st = forge_status(&s.repos, &h).await;
        forges.push(forge_dto(&h, Some(&st)));
    }
    Ok(Json(json!({ "local": local, "github": github, "workspaceDefault": ws_default, "jobs": jobs, "forges": forges, "scratch": scratch_settings })))
}

// ── Clone / fetch jobs ─────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneReq {
    #[serde(default)]
    repo: String,
    /// Clone a workspace repo by its id (repos-and-checkouts 4.3): the URL
    /// is rebuilt from the repo's canonical, for a host connected here.
    #[serde(default)]
    repo_id: Option<Uuid>,
    /// Clone the signed-in account's fork of `repo` instead, with `repo` as
    /// its `upstream`. Never creates a fork: no fork → 404.
    #[serde(default)]
    via_fork: bool,
    /// The connection (default: the URL's host, else github.com).
    #[serde(default)]
    forge: Option<String>,
}

/// How a clone/fetch job sets up `upstream` for a fork.
#[derive(Debug, Clone)]
pub struct ForkPlan {
    pub upstream: String,
    pub url: String,
    pub base: String,
}

/// The connection and repository a spec names: a URL's host picks the
/// connection; a bare `owner/name` uses `forge` (default github.com).
fn resolve_spec(s: &Shared, raw: &str, forge: Option<&str>) -> ApiResult<(ForgeHandle, RepoRef)> {
    let spec = ligus_forge::spec::parse_repo_spec(raw, true).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let h = match (spec.host.as_deref(), forge.map(str::trim).filter(|f| !f.is_empty())) {
        (Some(host), _) => s
            .repos
            .forges(&s.cfg.data_dir)
            .into_iter()
            .find(|h| ligus_forge::spec::same_host(&h.conn.host(), host))
            .ok_or_else(|| ApiError::bad_request(format!("{host} isn't a code host connected to this box: add it under Code hosts first")))?,
        (None, Some(id)) => {
            let id = ForgeId::parse(id).map_err(|e| ApiError::bad_request(e.to_string()))?;
            s.repos.forge(&s.cfg.data_dir, &id).ok_or_else(|| ApiError::not_found("code host connection"))?
        }
        (None, None) => s.repos.github(&s.cfg.data_dir).ok_or_else(|| ApiError::bad_request("no GitHub connection on this host: name a forge"))?,
    };
    if h.conn.kind != ForgeKind::GitLab && spec.repo.owner.contains('/') {
        return Err(ApiError::bad_request("give a repository as owner/name"));
    }
    if h.is_github_com() && (!repos::valid_owner(&spec.repo.owner) || !repos::valid_repo_name(&spec.repo.name)) {
        return Err(ApiError::bad_request(format!("'{}' isn't a valid GitHub repository name", spec.repo.full_name())));
    }
    Ok((h, spec.repo))
}

/// Where a repository clones to under the repos dir: `owner__name` on
/// github.com (as before forges), `<host>__<owner>__<name>` elsewhere
/// (GitLab subgroups joined with `__`).
pub fn checkout_for(h: &ForgeHandle, r: &RepoRef) -> String {
    if h.is_github_com() {
        return repos::checkout_name(&r.owner, &r.name);
    }
    let host = h.conn.host().replace(':', "-");
    format!("{host}__{}__{}", r.owner.replace('/', "__"), r.name)
}

/// The upstream setup for cloning `r`: its parent when the forge says it's
/// a fork (nothing when signed out or the forge can't tell).
async fn fork_plan(st: &RepoState, h: &ForgeHandle, r: &RepoRef) -> Option<ForkPlan> {
    if !forge_status(st, h).await.available {
        return None;
    }
    let (p, base) = h.forge.repo(r).await.ok()??.parent?;
    Some(ForkPlan { upstream: p.full_name(), url: h.forge.clone_url(&p, Transport::Https), base })
}

#[derive(Deserialize)]
pub struct ForkQuery {
    repo: String,
    #[serde(default)]
    forge: Option<String>,
}

/// `GET /v1/repos/fork?repo=owner/name[&forge=id]`: whether the signed-in
/// account has a fork of it (so the phone can offer "clone via my fork").
pub async fn fork_of(State(s): S, peer: Option<axum::Extension<super::tunnel::TunnelPeer>>, Query(q): Query<ForkQuery>) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
    let (h, repo) = resolve_spec(&s, &q.repo, q.forge.as_deref())?;
    let st = forge_status(&s.repos, &h).await;
    let Some(login) = st.login.filter(|_| st.available) else {
        return Err(err(
            StatusCode::PRECONDITION_FAILED,
            format!("{} isn't signed in on this host: {}", h.conn.display(), st.reason.unwrap_or_else(|| "run gh auth login on the host".into())),
        ));
    };
    let fork = h.forge.find_own_fork(&login, &repo).await.map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let repos_dir = repos::repos_dir(&s.cfg.data_dir);
    let fork = fork.map(|m| {
        let checkout = checkout_for(&h, &m.repo);
        let local = repos_dir.join(&checkout).join(".git").exists();
        json!({ "fullName": m.repo.full_name(), "checkout": checkout, "cloned": local })
    });
    Ok(Json(json!({ "repo": repo.full_name(), "login": login, "fork": fork, "forge": h.id().as_str() })))
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

/// A live workspace repo and the connection on this box for its host
/// (never a URL from the event: host and path re-validated, review F12).
fn resolve_by_id(s: &Shared, id: Uuid) -> ApiResult<(ligus_core::workspace_repo::WorkspaceRepo, ForgeHandle, RepoRef)> {
    let roster = s.lock().workspace_roster(s.ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let wr = roster.repos.get(&id).filter(|e| !e.removed).map(|e| e.repo.clone()).ok_or_else(|| ApiError::not_found("workspace repository"))?;
    let (host, rref) = ligus_runtime::repos::clone::clone_target(&wr).map_err(ApiError::bad_request)?;
    let h = s
        .repos
        .forges(&s.cfg.data_dir)
        .into_iter()
        .find(|h| ligus_forge::spec::same_host(&h.conn.host(), &host))
        .ok_or_else(|| ApiError::bad_request(format!("{host} isn't a code host connected to this box: add it under Code hosts first")))?;
    Ok((wr, h, rref))
}

pub async fn clone_repo(State(s): S, peer: Option<axum::Extension<super::tunnel::TunnelPeer>>, Json(req): Json<CloneReq>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    owner_gate(&s, peer.as_deref())?;
    let by_id = match req.repo_id {
        Some(id) => Some(resolve_by_id(&s, id)?),
        None => None,
    };
    let workspace_repo = by_id.as_ref().map(|(w, _, _)| w.clone());
    let (h, mut repo) = match by_id {
        Some((_, h, r)) => (h, r),
        None => resolve_spec(&s, &req.repo, req.forge.as_deref())?,
    };
    // Re-check the host's address and pin it for git (a name rebound to
    // internal space since the connection was added is refused).
    let pin = match &workspace_repo {
        Some(_) => ligus_forge::conn::git_resolve_pin(&h.conn).await.map_err(|e| ApiError::bad_request(e.to_string()))?,
        None => None,
    };
    // "Clone the parent via my fork": swap in the account's fork, or report
    // that there isn't one (this host never creates forks).
    if req.via_fork {
        let st = forge_status(&s.repos, &h).await;
        let Some(login) = st.login.filter(|_| st.available) else {
            return Err(err(StatusCode::PRECONDITION_FAILED, format!("cloning via your fork needs {}: {}", h.conn.display(), st.reason.unwrap_or_else(|| "run gh auth login on the host".into()))));
        };
        let found = h.forge.find_own_fork(&login, &repo).await.map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;
        let Some(f) = found else {
            return Err(ApiError::not_found(&format!(
                "a fork of {repo} under your account (this host doesn't create forks; fork it on {}, or clone {repo} directly)",
                h.conn.display()
            )));
        };
        repo = f.repo;
    }
    let full = repo.full_name();
    let checkout = match &workspace_repo {
        Some(w) => ligus_runtime::repos::clone_dir_name(w),
        None => checkout_for(&h, &repo),
    };
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
        let same = origin.as_deref().is_some_and(|o| {
            ligus_forge::spec::parse_remote(o).is_some_and(|(_, r)| r.same(&repo))
                || repos::parse_remote(o).is_some_and(|r| r.owner.eq_ignore_ascii_case(&repo.owner) && r.name.eq_ignore_ascii_case(&repo.name))
        });
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
    let url = h.forge.clone_url(&repo, Transport::Https);
    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let _cloning = workspace_repo.as_ref().map(|w| ligus_runtime::repos::start_cloning(w.id));
        let plan = rt.block_on(fork_plan(&shared.repos, &h, &repo));
        let mut auth = h.forge.git_auth_args();
        if workspace_repo.is_some() {
            auth.splice(0..0, ligus_runtime::repos::clone::by_id_transport_args(pin.as_deref()));
        }
        let result = run_clone_job(&shared, id, action, &url, &repos_dir, &dest, auth.clone())
            .and_then(|()| setup_fork(&shared, id, &dest, plan.as_ref(), action == "clone", &auth))
            .and_then(|()| match &workspace_repo {
                Some(w) => bind_clone(&shared, w, &dest, &checkout),
                None => Ok(()),
            });
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

/// After a clone by repo id: map it (confirmed, this host manages it),
/// check the repo's claimed root commit, and make it the box's workspace
/// default when there is none (review F18).
fn bind_clone(s: &Shared, w: &ligus_core::workspace_repo::WorkspaceRepo, dest: &FsPath, checkout: &str) -> Result<(), String> {
    // The clone's origin names the repo's identity; when the connection
    // cloned from a different URL (a mirror, an internal address), git keeps
    // using that one for transport (a local insteadOf).
    if ligus_runtime::repos::clone::finish_clone(dest, w)? == Some(false) {
        eprintln!("repos: {} doesn't contain the root commit the workspace entry claims", w.display_name);
    }
    {
        let svc = s.lock();
        ligus_runtime::repos::bind(svc.store(), svc.device_id(), &s.opts.label, s.ws, w, dest, ligus_runtime::event_store::CheckoutSource::Cloned)
            .map_err(|e| e.to_string())?;
    }
    crate::repos_cmd::bind_default_if_empty(&s.cfg.data_dir, checkout).map_err(|e| e.to_string())?;
    Ok(())
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
    ligus_core::process_util::harden_git(&mut c);
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
                .then(|| format!("stopped: the repository is larger than {} MB (LIGUS_REPOS_MAX_CLONE_MB)", limit / 1024 / 1024))
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
                 " (a private repository needs a signed-in code host on the host: gh auth login, or a token under Code hosts)"
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
    let mut v = blocking(move || chat_binding(&dir, chat.id).1).await?;
    tag_scratch(&s, chat.id, &mut v);
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
pub struct SetRepo {
    #[serde(default)]
    repo: Option<String>,
}

pub async fn put_chat_repo(State(s): S, peer: Option<axum::Extension<super::tunnel::TunnelPeer>>, Path(id): Path<String>, body: Bytes) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
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
            // One of this box's live scratch repositories, by folder name
            // (the phone's listing names them so): bound by its own path.
            let scratch = if d.exists() {
                None
            } else {
                let svc = s.lock();
                ligus_runtime::repos::scratch::live(svc.store(), s.ws).into_iter().find(|e| e.dir_name == name)
            };
            match scratch {
                Some(e) => Some(e.path.to_string_lossy().into_owned()),
                None => {
                    if !blocking(move || ligus_runtime::git_worktree::is_git_repo(&d)).await? {
                        return Err(ApiError::not_found(&format!("checkout '{name}' (clone it first)")));
                    }
                    Some(name)
                }
            }
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
    let mut v = blocking(move || chat_binding(&dir, cid).1).await?;
    tag_scratch(&s, chat.id, &mut v);
    if changed {
        // Which scratch repository (if any) lists this chat follows the binding.
        let svc = s.lock();
        let _ = ligus_runtime::repos::scratch::forget_chat(svc.store(), chat.id);
        if let Some(e) = target.as_deref().and_then(|t| ligus_runtime::repos::scratch::by_path(svc.store(), std::path::Path::new(t))) {
            let _ = ligus_runtime::repos::scratch::set_chat(svc.store(), e.id, chat.id, true, ligus_runtime::repos::scratch::now());
        }
        drop(svc);
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
    if !ligus_runtime::git_worktree::is_git_repo(&dir) {
        return Err(err(StatusCode::CONFLICT, "this chat's checkout is missing on this host"));
    }
    Ok((dir, binding_name(raw).0))
}

pub async fn pull_chat_repo(State(s): S, peer: Option<axum::Extension<super::tunnel::TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    // Fetches with the owner's forge auth and moves the box's checkout.
    owner_gate(&s, peer.as_deref())?;
    let chat = load_chat(&s, &id)?;
    ensure_can_write(&s, Some(&chat))?;
    let (dir, name) = bound_dir(&s, chat.id)?;
    let _busy = claim(&s.repos, &name)?;
    let d = dir.clone();
    let origin = blocking(move || repos::git_out(&d, &["remote", "get-url", "origin"])).await?.unwrap_or_default();
    let auth = s.repos.forge_of_remote(&s.cfg.data_dir, &origin).map(|(h, _)| h.forge.git_auth_args()).unwrap_or_default();
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

pub async fn open_pr(State(s): S, peer: Option<axum::Extension<super::tunnel::TunnelPeer>>, Path(id): Path<String>, Json(req): Json<PrReq>) -> ApiResult<Json<Value>> {
    // Pushes and opens the PR as the owner, with their forge credentials.
    owner_gate(&s, peer.as_deref())?;
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
    let _busy = claim(&s.repos, &name)?;
    let branch = repos::chat_branch(chat.id);
    let conflict = |m: String| err(StatusCode::CONFLICT, m);
    let d = dir.clone();
    // The configured URL: the PR is opened on that repository, whatever
    // insteadOf rewrites say.
    let origin = blocking(move || ligus_runtime::ligus_mcp::forge_tools::configured_remote_url(&d, "origin")).await?.unwrap_or_default();
    let (h, remote) = s
        .repos
        .forge_of_remote(&s.cfg.data_dir, &origin)
        .ok_or_else(|| conflict("this checkout's origin isn't on a code host connected to this box".into()))?;
    let st = forge_status(&s.repos, &h).await;
    if !st.available {
        return Err(err(
            StatusCode::PRECONDITION_FAILED,
            format!("{} isn't signed in on this host: {}", h.conn.display(), st.reason.unwrap_or_else(|| "run gh auth login on the host".into())),
        ));
    }
    // What the forge says about origin (a fork without `upstream` would open
    // the PR against the fork itself).
    let origin_meta = h.forge.repo(&remote).await.ok().flatten();
    let shared: Arc<Shared> = s.clone();
    let (chat_id, chat_title) = (chat.id, chat.title.clone());
    let auth = h.forge.git_auth_args();
    let b = branch.clone();
    let remote_c = remote.clone();
    let origin_url = origin.clone();
    // Git work (checks, fetch, push) on a blocking thread; nothing is pushed
    // to the upstream project, and nothing is ever forced.
    let pushed = blocking(move || -> Result<Pushed, (StatusCode, String)> {
        let branch = b;
        let conflict = |m: String| (StatusCode::CONFLICT, m);
        if !repos::ref_exists(&dir, &format!("refs/heads/{branch}")) {
            return Err(conflict(format!(
                "this chat has no commits on {branch} yet: approve a proposal first (the host commits approved changes \
                 when LIGUS_SERVE_APPLY_ON_APPROVE=1)"
            )));
        }
        let info = repos::inspect(&dir);
        let Some(fork) = info.fork else {
            if let Some((p, _)) = origin_meta.as_ref().and_then(|m| m.parent.clone()) {
                return Err(conflict(format!(
                    "{} is a fork of {p} but this checkout has no upstream remote: fetch it from the repo \
                     picker (that adds upstream), then open the PR",
                    remote_c.full_name()
                )));
            }
            // The PR opens on origin's repository: every push URL (after
            // pushurl and pushInsteadOf) must be that repository, or the
            // branch lands somewhere the PR doesn't point.
            let target = repos::check_push_target(&dir, "origin", None).map_err(conflict)?;
            let fetch_host = repos::parse_remote(&origin_url).map(|r| r.host).unwrap_or_default();
            let same_host = target.host.is_empty() || fetch_host.is_empty() || ligus_forge::spec::same_host(&target.host, &fetch_host);
            if !(same_host && target.owner.eq_ignore_ascii_case(&remote_c.owner) && target.name.eq_ignore_ascii_case(&remote_c.name)) {
                return Err(conflict(format!("origin pushes to {}, not {}: fix the remote first", target.full_name(), remote_c.full_name())));
            }
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
            return Ok(Pushed { base_repo: remote_c.clone(), base, pushed: branch, fork: false });
        };
        // A fork: push to origin (never upstream) under a readable branch,
        // then open the PR on the upstream project.
        let target = repos::check_push_target(&dir, "origin", Some(&fork.upstream)).map_err(conflict)?;
        if !(target.owner.eq_ignore_ascii_case(&remote_c.owner) && target.name.eq_ignore_ascii_case(&remote_c.name)) {
            return Err(conflict(format!("origin pushes to {}, not the fork {}: fix the remote first", target.full_name(), remote_c.full_name())));
        }
        let layout = ligus_forge::RepoLayout { origin: remote_c.clone(), upstream: Some(RepoRef::new(fork.upstream.owner.clone(), fork.upstream.name.clone())) };
        ligus_forge::push_remote(&layout).map_err(|e| conflict(e.to_string()))?;
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
        Ok(Pushed { base_repo: layout.upstream.expect("set"), base, pushed, fork: true })
    })
    .await?
    .map_err(|(c, m)| err(c, m))?;
    let draft = req.draft.unwrap_or(pushed.fork);
    let d = ChangeRequestDraft {
        base: pushed.base_repo.clone(),
        base_branch: pushed.base.clone(),
        head: remote.clone(),
        head_branch: pushed.pushed.clone(),
        title,
        body,
        draft,
    };
    let ChangeRequestOutcome::Opened { url, existing, .. } = h.forge.open_change_request(&d).await.map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let repo = pushed.base_repo.full_name();
    let kind = h.conn.kind.change_request_kind();
    let noun = if kind == ligus_forge::ChangeRequestKind::MergeRequest { "merge request" } else { "pull request" };
    let note = if existing {
        format!("{} (already open, branch updated): {url}", capitalize(noun))
    } else if draft {
        format!("Opened {noun}: {url} (draft, on {repo})")
    } else {
        format!("Opened {noun}: {url}")
    };
    s.lock().post_system_note(chat.id, chat.workspace_id, note)?;
    s.after_write();
    Ok(Json(json!({
        "url": url,
        "existing": existing,
        "branch": repos::chat_branch(chat.id),
        "pushedBranch": pushed.pushed,
        "repo": repo,
        "base": pushed.base,
        "draft": draft,
        "kind": kind.as_str(),
        "forge": h.id().as_str(),
    })))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// What the git half of opening a PR did.
struct Pushed {
    /// The repository the PR opens on (upstream for a fork).
    base_repo: RepoRef,
    base: String,
    /// The branch name on the remote the PR's head was pushed as.
    pushed: String,
    fork: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final review (area B): the box's clone and fetch git drops inherited
    /// repo redirects (`GIT_DIR`, `GIT_INDEX_FILE`, ...) like Ligus's own git.
    #[test]
    fn final_review_quiet_git_is_the_hardened_git() {
        let c = quiet("git");
        let removed: Vec<String> = c.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_string_lossy().into_owned()).collect();
        for k in ["GIT_DIR", "GIT_INDEX_FILE", "GIT_EXTERNAL_DIFF"] {
            assert!(removed.iter().any(|r| r == k), "{k} inherited: {removed:?}");
        }
    }

    #[test]
    fn progress_lines_parse() {
        assert_eq!(parse_progress("Receiving objects:  45% (450/1000), 1.2 MiB | 2 MiB/s"), Some(("Receiving objects".into(), Some(45))));
        assert_eq!(parse_progress("remote: Counting objects: 100% (5/5), done."), Some(("Counting objects".into(), Some(100))));
        assert_eq!(parse_progress("Cloning into '/x/.clone-1'..."), None);
        assert_eq!(parse_progress("fatal: repository not found"), None);
    }

    /// A command that runs for about five seconds (Windows has no `sleep`).
    fn five_seconds() -> Command {
        if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "6", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("5");
            c
        }
    }

    #[test]
    fn run_times_out_and_kills() {
        let c = five_seconds();
        let t = Instant::now();
        let e = run(c, Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn check_can_stop_a_command() {
        let c = five_seconds();
        let e = run_watched(c, Duration::from_secs(10), |_| {}, || Some("too big".into())).unwrap_err();
        assert_eq!(e, "too big");
    }

    /// Test builds never reach a real code host or the developer's own
    /// `gh` login: the default github.com connection is anonymous offline,
    /// and statuses never go to the network.
    #[tokio::test]
    async fn test_builds_never_contact_code_hosts() {
        let st = RepoState::default();
        assert!(!st.online());
        let dir = tempfile::tempdir().unwrap();
        let all = st.forges(dir.path());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].forge.auth_source(), ligus_forge::AuthSource::None, "not the gh login");
        assert!(!all[0].forge.has_write());
        let s = forge_status(&st, &all[0]).await;
        assert_eq!(s.reason.as_deref(), Some(OFFLINE));
        assert_eq!(checkout_for(&all[0], &RepoRef::new("o", "n")), "o__n", "github.com keeps owner__name");
    }

    #[test]
    fn absolute_bindings_never_expose_paths() {
        assert_eq!(binding_name("/home/x/secret/repo"), ("repo".into(), true));
        assert_eq!(binding_name("sandbox"), ("sandbox".into(), false));
    }
}

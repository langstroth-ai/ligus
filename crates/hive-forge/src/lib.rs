//! Forges for Ligus repo work: one [`Forge`] trait over GitHub (github.com
//! and GHES), GitLab (gitlab.com and self-managed) and Forgejo, Gitea and
//! Codeberg (one implementation), behind a per-instance base URL.
//!
//! This crate is pure HTTP plus git argv builders: no Tauri, no axum, no
//! `hive-runtime`. Hosts (`ligus serve`, the desktop) build forges from the
//! device-local connection list ([`conn`]) and hand them a
//! [`tokens::CredentialSource`].
//!
//! Rules this crate keeps:
//! - **Tokens stay on the device that does the git work.** They live in the
//!   OS secret store behind [`tokens::TokenStore`], are read only when a
//!   request or a push needs them, and never appear in the connection file,
//!   argv, a child's environment, an error message or a `Debug` print
//!   ([`tokens::Secret`]).
//! - **Read and write are separate.** Listing and agent tools use the read
//!   token (or the write token when it is the only one, on the host only);
//!   pushes, forks and change requests use the write token only.
//! - **Writes run only on an explicit human action** (or an applied
//!   proposal); the agent-facing surface ([`ForgeReader`]) has no write
//!   method at all.
//! - **Tokens only travel over TLS.** A connection's base URL is `https`,
//!   or `http` to a loopback address (a forge on this machine).

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod conn;
pub mod device;
pub mod gitea;
pub mod github;
pub mod gitlab;
mod http;
pub mod spec;
pub mod tokens;

pub use conn::{Connection, Connections};
pub use tokens::{AuthSource, CredentialSource, Purpose, Secret, TokenStore};

/// Environment variables forge CLIs and community MCP servers read tokens
/// from. None of them ever reaches an agent's sandbox: the child env is an
/// allow-list (`hive_core::process_util::child_env`), and
/// `forge_token_env_never_reaches_a_child` pins that these names are not on
/// it.
pub const FORGE_TOKEN_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITHUB_PERSONAL_ACCESS_TOKEN",
    "GITLAB_TOKEN",
    "GITLAB_PERSONAL_ACCESS_TOKEN",
    "GITEA_TOKEN",
    "FORGEJO_TOKEN",
    "CODEBERG_TOKEN",
    "SRHT_TOKEN",
];

// ── Errors ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    /// No credentials for what was asked (signed out, or no write token).
    #[error("{0}")]
    NotSignedIn(String),
    /// The forge refused the credentials (401/403).
    #[error("{0}")]
    Unauthorized(String),
    #[error("not found: {0}")]
    NotFound(String),
    /// The request was well formed but the forge said no (422, 409, …).
    #[error("{0}")]
    Rejected(String),
    /// Not something this forge (or this build) can do.
    #[error("{0}")]
    Unsupported(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    /// Network, TLS, a 5xx, or an answer that didn't parse.
    #[error("{0}")]
    Transport(String),
    #[error("secret store: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, ForgeError>;

// ── Identity of a connection ───────────────────────────────────────────────

/// Which implementation a connection uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    GitHub,
    GitLab,
    /// Forgejo, Gitea and Codeberg (one API).
    Gitea,
}

impl ForgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "github",
            ForgeKind::GitLab => "gitlab",
            ForgeKind::Gitea => "gitea",
        }
    }

    /// `github`, `gitlab`, or any of `gitea` / `forgejo` / `codeberg`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "github" | "ghes" => Some(ForgeKind::GitHub),
            "gitlab" => Some(ForgeKind::GitLab),
            "gitea" | "forgejo" | "codeberg" => Some(ForgeKind::Gitea),
            _ => None,
        }
    }

    /// What a change request is called here.
    pub fn change_request_kind(self) -> ChangeRequestKind {
        match self {
            ForgeKind::GitLab => ChangeRequestKind::MergeRequest,
            _ => ChangeRequestKind::PullRequest,
        }
    }

    /// The label of the "open" button (spec 6.4).
    pub fn open_label(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "Open draft PR",
            ForgeKind::GitLab => "Open draft MR",
            ForgeKind::Gitea => "Open WIP PR",
        }
    }
}

/// A connection id: `<kind>:<host>` (`github:github.com`,
/// `gitlab:gitlab.example.com`, `gitea:codeberg.org`). `github.com` alone is
/// accepted as an alias of `github:github.com` (the shipped phone app's
/// default).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ForgeId(pub String);

impl ForgeId {
    pub fn new(kind: ForgeKind, host: &str) -> Self {
        Self(format!("{}:{}", kind.as_str(), host.to_ascii_lowercase()))
    }

    /// The default GitHub connection.
    pub fn github_com() -> Self {
        Self("github:github.com".into())
    }

    /// An id from user or API input: `kind:host`, or a bare `github.com`.
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("github.com") || raw.eq_ignore_ascii_case("github") {
            return Ok(Self::github_com());
        }
        let (kind, host) = raw.split_once(':').ok_or_else(|| ForgeError::Invalid(format!("'{raw}' isn't a forge connection id (kind:host)")))?;
        let kind = ForgeKind::parse(kind).ok_or_else(|| ForgeError::Invalid(format!("unknown forge kind '{kind}'")))?;
        if !spec::valid_host(host) {
            return Err(ForgeError::Invalid(format!("'{host}' isn't a host name")));
        }
        Ok(Self::new(kind, host))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ForgeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── Repositories ───────────────────────────────────────────────────────────

/// A repository on a forge. `owner` may contain `/` on GitLab (nested
/// groups: `group/sub`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self { owner: owner.into(), name: name.into() }
    }
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
    /// Same repository, compared case-insensitively.
    pub fn same(&self, other: &RepoRef) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }
}

impl std::fmt::Display for RepoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// One row of a repository listing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSummary {
    pub repo: RepoRef,
    pub description: String,
    pub private: bool,
    pub default_branch: String,
    pub updated_at: String,
    pub web_url: String,
    pub fork: bool,
}

/// A repository's metadata: default branch, fork parent, push access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoMeta {
    pub repo: RepoRef,
    pub default_branch: String,
    /// The repository this one is a fork of, with its default branch.
    pub parent: Option<(RepoRef, String)>,
    pub private: bool,
    pub web_url: String,
    /// The signed-in account can push (`None` = the forge didn't say).
    pub can_push: Option<bool>,
}

impl RepoMeta {
    pub fn is_fork(&self) -> bool {
        self.parent.is_some()
    }
}

#[derive(Debug, Clone, Default)]
pub struct RepoQuery {
    /// Search text.
    pub q: Option<String>,
    /// Narrow to an account, org or group.
    pub owner: Option<String>,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Https,
    Ssh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeUser {
    /// Stable numeric id (as a string).
    pub id: String,
    pub login: String,
    pub name: String,
}

/// What a connection can do (the UI and the MCP tools read this).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeCaps {
    /// A real draft flag on create (GitHub); else a title prefix.
    pub draft_cr: bool,
    pub fork_api: bool,
    pub ci_status: bool,
    pub review_threads: bool,
    pub issues: bool,
    /// Change requests open through the API (SourceHut: no).
    pub api_open_cr: bool,
    /// A device sign-in flow exists for this kind (whether it's configured
    /// is the connection's business).
    pub device_flow: bool,
}

// ── Change requests ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeRequestKind {
    PullRequest,
    MergeRequest,
}

impl ChangeRequestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeRequestKind::PullRequest => "pr",
            ChangeRequestKind::MergeRequest => "mr",
        }
    }
}

/// A change request to open: `head`'s `head_branch` into `base`'s
/// `base_branch` (`head` is a fork of `base`, or `base` itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestDraft {
    pub base: RepoRef,
    pub base_branch: String,
    pub head: RepoRef,
    pub head_branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum ChangeRequestOutcome {
    Opened {
        number: String,
        url: String,
        /// The forge has a real draft flag (else the title carried a prefix).
        draft_supported: bool,
        /// One was already open for this head; the push updated it.
        existing: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRequest {
    pub number: String,
    pub title: String,
    pub state: CrState,
    pub draft: bool,
    pub author: String,
    pub head: String,
    pub base: String,
    pub url: String,
    pub updated_at: String,
    /// Description (bounded by the reader).
    pub body: String,
    /// The head commit, when the forge gave it (for CI).
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StateFilter {
    #[default]
    Open,
    Closed,
    All,
}

impl StateFilter {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "open" | "opened" => Some(StateFilter::Open),
            "closed" | "merged" => Some(StateFilter::Closed),
            "all" => Some(StateFilter::All),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    pub author: String,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub created_at: String,
    pub resolved: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub number: String,
    pub title: String,
    pub state: String,
    pub author: String,
    pub url: String,
    pub labels: Vec<String>,
    pub body: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CiState {
    Pending,
    Running,
    Success,
    Failure,
    Cancelled,
    Unknown,
}

impl CiState {
    /// The worst of two states (failure beats running beats pending beats
    /// success).
    pub fn worst(self, other: CiState) -> CiState {
        let rank = |s: CiState| match s {
            CiState::Failure => 5,
            CiState::Cancelled => 4,
            CiState::Running => 3,
            CiState::Pending => 2,
            CiState::Unknown => 1,
            CiState::Success => 0,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CiCheck {
    pub name: String,
    pub state: CiState,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CiStatus {
    pub state: CiState,
    pub checks: Vec<CiCheck>,
    /// Set when part of CI couldn't be read with this credential (GitHub's
    /// Checks API refuses fine-grained tokens): "checks unavailable with
    /// this token".
    pub note: Option<String>,
}

impl CiStatus {
    pub fn from_checks(checks: Vec<CiCheck>, note: Option<String>) -> Self {
        let state = if checks.is_empty() { CiState::Unknown } else { checks.iter().fold(CiState::Success, |a, c| a.worst(c.state)) };
        Self { state, checks, note }
    }
}

/// A file at a ref (text only; binary files are refused by the reader).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAtRef {
    pub path: String,
    pub git_ref: String,
    pub text: String,
    /// The file was longer than the cap and was cut.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyUsage {
    Auth,
    Signing,
    Both,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicKeyEntry {
    pub openssh: String,
    pub usage: KeyUsage,
}

/// Where pushes from a checkout go: always the fork when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoLayout {
    /// What `origin` points at.
    pub origin: RepoRef,
    /// The parent project (`upstream`, fetch-only), for a fork checkout.
    pub upstream: Option<RepoRef>,
}

/// The remote a change request's head is pushed to: `origin`, and never a
/// remote that resolves to the upstream project.
pub fn push_remote(layout: &RepoLayout) -> Result<&'static str> {
    if layout.upstream.as_ref().is_some_and(|u| u.same(&layout.origin)) {
        return Err(ForgeError::Rejected(format!(
            "refusing to push: origin points at {}, the upstream project; pushes go only to your fork",
            layout.origin
        )));
    }
    Ok("origin")
}

/// Cap on a read result's text fields (bodies, files), in characters.
pub const MAX_TEXT_CHARS: usize = 64 * 1024;

pub(crate) fn cap_text(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        (s.to_string(), false)
    } else {
        (s.chars().take(max).collect(), true)
    }
}

// ── The trait ──────────────────────────────────────────────────────────────

/// The read side, which is all an agent ever reaches (through the Ligus MCP
/// server, in the host process). There is no write method here on purpose:
/// `agent_tools_have_no_forge_writes`.
#[async_trait]
pub trait ForgeReader: Send + Sync {
    fn kind(&self) -> ForgeKind;
    fn id(&self) -> &ForgeId;
    async fn list_change_requests(&self, r: &RepoRef, state: StateFilter, limit: u32) -> Result<Vec<ChangeRequest>>;
    async fn change_request(&self, r: &RepoRef, number: &str) -> Result<ChangeRequest>;
    async fn review_comments(&self, r: &RepoRef, number: &str) -> Result<Vec<ReviewComment>>;
    async fn issues(&self, r: &RepoRef, state: StateFilter, limit: u32) -> Result<Vec<Issue>>;
    async fn issue(&self, r: &RepoRef, number: &str) -> Result<Issue>;
    async fn ci_status(&self, r: &RepoRef, git_ref: &str) -> Result<CiStatus>;
    async fn file_at(&self, r: &RepoRef, path: &str, git_ref: &str) -> Result<FileAtRef>;
}

/// A forge connection with its credentials: reads, plus the host-only
/// writes (fork, change request) and the git wiring.
#[async_trait]
pub trait Forge: ForgeReader {
    /// The web base (`https://github.com`, `https://gitlab.example.com`).
    fn web_base(&self) -> &str;
    fn capabilities(&self) -> ForgeCaps;
    fn auth_source(&self) -> AuthSource;

    /// The signed-in account (`None` = anonymous).
    async fn whoami(&self) -> Result<Option<ForgeUser>>;
    async fn list_repos(&self, q: &RepoQuery, login: &str) -> Result<Vec<RepoSummary>>;
    /// The signed-in account and its orgs or groups.
    async fn owners(&self, login: &str) -> Result<Vec<String>>;
    /// `Ok(None)` when there is no such repository (or it isn't visible).
    async fn repo(&self, r: &RepoRef) -> Result<Option<RepoMeta>>;
    /// The signed-in account's fork of `upstream`: the same name under the
    /// login, reported by the forge as a fork of exactly `upstream`.
    async fn find_own_fork(&self, login: &str, upstream: &RepoRef) -> Result<Option<RepoMeta>> {
        if login.eq_ignore_ascii_case(&upstream.owner) {
            return Ok(None);
        }
        Ok(self.repo(&RepoRef::new(login, upstream.name.clone())).await?.filter(|m| m.parent.as_ref().is_some_and(|(p, _)| p.same(upstream))))
    }
    /// Only on an explicit human action.
    async fn create_fork(&self, upstream: &RepoRef) -> Result<RepoRef>;
    /// Only on an explicit human action or an applied proposal.
    async fn open_change_request(&self, d: &ChangeRequestDraft) -> Result<ChangeRequestOutcome>;

    fn clone_url(&self, r: &RepoRef, transport: Transport) -> String {
        let host = spec::host_of(self.web_base()).unwrap_or_default();
        match transport {
            Transport::Https => format!("{}/{}.git", self.web_base().trim_end_matches('/'), r.full_name()),
            Transport::Ssh => format!("git@{host}:{}.git", r.full_name()),
        }
    }
    /// The repository a remote URL points at, when it is on this forge.
    fn repo_of_remote(&self, url: &str) -> Option<RepoRef> {
        let host = spec::host_of(self.web_base())?;
        let (h, r) = spec::parse_remote(url)?;
        spec::same_host(&h, &host).then_some(r)
    }
    /// `git -c …` args that let the host's git calls use this connection's
    /// write credential (none when signed out). The token itself is never
    /// in them: a credential helper reads it from the store at push time.
    fn git_auth_args(&self) -> Vec<String>;
}

/// Where the `ligus` binary is, for the `!ligus forge git-credential <id>`
/// helper ([`tokens::store_helper_args`]).
#[derive(Debug, Clone)]
pub struct HelperProgram(pub PathBuf);

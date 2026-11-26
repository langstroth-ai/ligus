//! Workspace repos on this device (docs/design/v2/repos-and-checkouts.md):
//! the one host abstraction the desktop, `ligus serve` and `ligus worker`
//! share for checkouts.
//!
//! * **Mappings** ([`bind`], [`detect`], [`unbind`]): which folder on this
//!   device holds which workspace repo (`repo_checkouts` in `ligus.db`, never
//!   synced). An explicit bind or a clone is confirmed; a detected match
//!   isn't until a person says so.
//! * **The turn gate** ([`turn_checkout`]): the host that runs a turn is the
//!   only gate (review F8). A chat with a repo runs only in a confirmed
//!   checkout whose remote still is that repo, managed by this host;
//!   anything else refuses with a reason instead of running in the wrong
//!   code.
//! * **The chat branch** ([`prepare_chat`]): cut from the pinned base
//!   (`ChatBasePinned`) or the chat's base branch, then brought up to date
//!   by replaying the chat's ledger (review F3).
//! * **Apply** ([`apply::apply_to_chat_branch`]): in a scratch index or
//!   worktree, then a compare-and-swap of the chat branch (review F4).
//! * **Coverage** ([`coverage`]): advisory, on the host's sealed presence
//!   beat; never paths.

pub mod apply;
pub mod clone;
pub mod git;
pub mod marker;
pub mod scan;
pub mod scratch;
pub mod scratch_host;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ligus_core::authz::RosterState;
use ligus_core::workspace_repo::{CoverageEntry, RepoEntry, WorkspaceRepo};
use ligus_core::ChatSession;
use uuid::Uuid;

use crate::event_store::{CheckoutSource, EventStore, RepoMapping};

pub use git::{Aliases, MatchVia};

/// The workspace's repos as the config roster holds them (removed ones
/// included, as tombstones), by display name.
pub fn workspace_repos(roster: &RosterState) -> Vec<RepoEntry> {
    let mut v: Vec<RepoEntry> = roster.repos.values().cloned().collect();
    v.sort_by(|a, b| {
        a.repo
            .display_name
            .to_lowercase()
            .cmp(&b.repo.display_name.to_lowercase())
            .then(a.repo.id.cmp(&b.repo.id))
    });
    v
}

/// The live repos.
pub fn live_repos(roster: &RosterState) -> Vec<WorkspaceRepo> {
    workspace_repos(roster)
        .into_iter()
        .filter(|r| !r.removed)
        .map(|r| r.repo)
        .collect()
}

/// Why a folder can't be mapped to a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    NotACheckout,
    /// A linked worktree (a chat's, say): map its main checkout instead.
    LinkedWorktree,
    /// Its remotes aren't the repo (what they are instead, when known).
    NotThisRepo {
        found: Option<String>,
    },
    /// Another host on this machine manages it (review F11).
    ManagedElsewhere {
        label: String,
    },
    /// The folder is already mapped to another repo in this workspace.
    FolderTaken,
    Store(String),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::NotACheckout => f.write_str("that folder isn't a git checkout"),
            BindError::LinkedWorktree => {
                f.write_str("that folder is a linked worktree; pick its main checkout")
            }
            BindError::NotThisRepo { found: Some(c) } => {
                write!(f, "that checkout is {c}, not this repository")
            }
            BindError::NotThisRepo { found: None } => {
                f.write_str("that checkout's remotes aren't this repository")
            }
            BindError::ManagedElsewhere { label } => {
                write!(f, "that checkout is managed by {label}; clone the repository separately to work on it from here")
            }
            BindError::FolderTaken => {
                f.write_str("that folder is already mapped to another repository in this workspace")
            }
            BindError::Store(e) => write!(f, "couldn't record the mapping: {e}"),
        }
    }
}

impl std::error::Error for BindError {}

/// The aliases this device matches with in `workspace_id` (design 12):
/// its own SSH host aliases, plus each synced alias a person here accepted,
/// while the workspace (`roster`) still names it with the same target.
/// Without a roster only the device's own aliases count: an acceptance is
/// never used unless the workspace still names that alias for that target.
pub fn aliases(store: &EventStore, workspace_id: Uuid, roster: Option<&RosterState>) -> Aliases {
    let mut out = Aliases {
        device: store.ssh_host_aliases().unwrap_or_default(),
        ..Default::default()
    };
    let Some(r) = roster else { return out };
    for (kind, alias, target) in store.accepted_aliases(workspace_id).unwrap_or_default() {
        match kind.as_str() {
            "host" if r.host_aliases.get(&alias) == Some(&target) => {
                out.hosts.insert(alias, target);
            }
            "repo" => {
                let Ok(repo) = Uuid::parse_str(&target) else {
                    continue;
                };
                if r.repo_live(repo)
                    && r.repo_aliases
                        .get(&repo)
                        .is_some_and(|l| l.contains(&alias))
                {
                    out.repos.entry(repo).or_default().push(alias);
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether the checkout at `dir` is `repo`: by remote, or for a `local:`
/// repo by its root commit (membership, never recomputed).
pub fn checkout_matches(dir: &Path, repo: &WorkspaceRepo, aliases: &Aliases) -> Option<MatchVia> {
    checkout_match(dir, repo, aliases).map(|(v, _)| v)
}

/// [`checkout_matches`], and whether an accepted synced alias was needed
/// ("via alias").
pub fn checkout_match(
    dir: &Path,
    repo: &WorkspaceRepo,
    aliases: &Aliases,
) -> Option<(MatchVia, bool)> {
    if let Some(root) = repo.canonical.strip_prefix("local:") {
        return git::has_root(dir, root).then_some((MatchVia::Origin, false));
    }
    git::match_repo_via(&git::remotes(dir), repo, aliases)
}

fn canon_path(p: &Path) -> PathBuf {
    git::canonical(p)
}

/// Map the checkout at `path` to `repo` on this device (a person picked it,
/// or this device cloned it). It must be a main checkout whose remote is the
/// repo, and not managed by another host on this machine; this host claims
/// it.
pub fn bind(
    store: &EventStore,
    me: Uuid,
    host_label: &str,
    workspace_id: Uuid,
    repo: &WorkspaceRepo,
    path: &Path,
    source: CheckoutSource,
) -> Result<RepoMapping, BindError> {
    // The workspace as this device folds it now, so an accepted synced alias
    // counts only while the workspace still names it (design 12).
    let roster = store
        .load_config(workspace_id, None, None)
        .ok()
        .map(|(_, t)| t.current().clone());
    bind_in(
        store,
        roster.as_ref(),
        me,
        host_label,
        workspace_id,
        repo,
        path,
        source,
    )
}

/// [`bind`] against a roster the caller already holds.
#[allow(clippy::too_many_arguments)]
pub fn bind_in(
    store: &EventStore,
    roster: Option<&RosterState>,
    me: Uuid,
    host_label: &str,
    workspace_id: Uuid,
    repo: &WorkspaceRepo,
    path: &Path,
    source: CheckoutSource,
) -> Result<RepoMapping, BindError> {
    if !crate::git_worktree::is_git_repo(path) {
        return Err(BindError::NotACheckout);
    }
    let path = canon_path(path);
    if std::fs::symlink_metadata(path.join(".git")).is_ok_and(|m| m.is_file()) {
        return Err(BindError::LinkedWorktree);
    }
    let aliases = aliases(store, workspace_id, roster);
    let Some(via) = checkout_matches(&path, repo, &aliases) else {
        return Err(BindError::NotThisRepo {
            found: git::suggested_canonical(&path, &aliases.all_hosts()).map(|(c, _)| c),
        });
    };
    match marker::claim(&path, me, host_label) {
        Ok(marker::Manager::Me) => {}
        Ok(marker::Manager::Other { label, .. }) => {
            return Err(BindError::ManagedElsewhere { label })
        }
        Err(e) => return Err(BindError::Store(e.to_string())),
    }
    let m = RepoMapping {
        workspace_id,
        repo_id: repo.id,
        path,
        source,
        primary: false,
        confirmed: true,
        via_fork: via == MatchVia::Upstream,
    };
    if !store
        .put_repo_mapping(&m)
        .map_err(|e| BindError::Store(e.to_string()))?
    {
        return Err(BindError::FolderTaken);
    }
    Ok(store
        .repo_mappings(workspace_id)
        .ok()
        .and_then(|v| {
            v.into_iter()
                .find(|x| x.repo_id == m.repo_id && x.path == m.path)
        })
        .unwrap_or(m))
}

/// A person confirmed a detected match ("use it"): re-checked like a bind.
pub fn confirm(
    store: &EventStore,
    me: Uuid,
    host_label: &str,
    workspace_id: Uuid,
    repo: &WorkspaceRepo,
    path: &Path,
) -> Result<RepoMapping, BindError> {
    bind(
        store,
        me,
        host_label,
        workspace_id,
        repo,
        path,
        CheckoutSource::Detected,
    )
}

/// Forget a mapping; give the checkout's marker up when nothing else here
/// maps it.
pub fn unbind(
    store: &EventStore,
    me: Uuid,
    workspace_id: Uuid,
    repo_id: Uuid,
    path: &Path,
) -> crate::event_store::Result<()> {
    store.remove_repo_mapping(workspace_id, repo_id, path)?;
    let still = store
        .repo_mappings(workspace_id)?
        .iter()
        .any(|m| m.path == path);
    if !still {
        marker::release(path, me);
    }
    Ok(())
}

/// Scan this device's roots (plus `extra`, such as the box's repos dir)
/// for checkouts of the workspace's live repos and record each new match,
/// unconfirmed. Returns what it found (also already-known ones).
pub fn detect(
    store: &EventStore,
    workspace_id: Uuid,
    roster: &RosterState,
    extra: &[PathBuf],
) -> crate::event_store::Result<Vec<scan::Found>> {
    let mut roots = store.scan_roots()?;
    roots.extend(extra.iter().cloned());
    let aliases = aliases(store, workspace_id, Some(roster));
    let found = scan::scan(&roots, &live_repos(roster), &aliases);
    record_found(store, workspace_id, &found)?;
    Ok(found)
}

/// Record what a scan found (unconfirmed), the store-only half of [`detect`]
/// for callers that scan without holding a lock.
pub fn record_found(
    store: &EventStore,
    workspace_id: Uuid,
    found: &[scan::Found],
) -> crate::event_store::Result<()> {
    for f in found {
        let m = RepoMapping {
            workspace_id,
            repo_id: f.repo_id,
            path: canon_path(&f.path),
            source: CheckoutSource::Detected,
            primary: false,
            confirmed: false,
            via_fork: f.via_fork,
        };
        let _ = store.put_repo_mapping(&m)?;
    }
    Ok(())
}

/// A clone's folder name for `repo` (review F13): `<display-slug>-<first 8
/// hex of the repo id>`, so two repos never share a folder whatever their
/// names or case.
pub fn clone_dir_name(repo: &WorkspaceRepo) -> String {
    let slug: String = repo
        .display_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug: String = slug.chars().take(40).collect();
    let hex = repo.id.simple().to_string();
    if slug.is_empty() {
        format!("repo-{}", &hex[..8])
    } else {
        format!("{slug}-{}", &hex[..8])
    }
}

// ── Clones in flight on this host ────────────────────────────────────────

fn cloning_set() -> &'static std::sync::Mutex<HashSet<Uuid>> {
    static S: std::sync::OnceLock<std::sync::Mutex<HashSet<Uuid>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// The repos this process is cloning now (coverage says `cloning`, and a
/// turn in a chat on one of them waits, already claimed).
pub fn cloning() -> HashSet<Uuid> {
    cloning_set()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Mark `repo` as being cloned until the guard drops.
pub fn start_cloning(repo: Uuid) -> CloningGuard {
    cloning_set()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(repo);
    CloningGuard(repo)
}

pub struct CloningGuard(Uuid);

impl Drop for CloningGuard {
    fn drop(&mut self) {
        cloning_set()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

// ── Coverage ─────────────────────────────────────────────────────────────

fn coverage_key(workspace_id: Uuid) -> String {
    format!("coverage:{workspace_id}")
}

/// Whether this device reports its coverage in `workspace_id` (review
/// Q6): a person's explicit choice, else `default_on` (on for worker hosts
/// and the workspace owner's own devices).
pub fn coverage_reporting(store: &EventStore, workspace_id: Uuid, default_on: bool) -> bool {
    match store
        .repo_setting(&coverage_key(workspace_id))
        .ok()
        .flatten()
        .as_deref()
    {
        Some("on") => true,
        Some("off") => false,
        _ => default_on,
    }
}

pub fn set_coverage_reporting(
    store: &EventStore,
    workspace_id: Uuid,
    on: Option<bool>,
) -> crate::event_store::Result<()> {
    store.set_repo_setting(
        &coverage_key(workspace_id),
        on.map(|b| if b { "on" } else { "off" }),
    )
}

/// This device's coverage of `repos` (what its beat carries): `present`
/// for a confirmed checkout that still is the repo, `cloning` while a clone
/// runs, `error`/`remote_mismatch` for a mapped checkout whose remote moved,
/// `missing` otherwise (code `unconfirmed` when a scan found one nobody
/// confirmed yet). Never a path, a remote or a branch.
pub fn coverage(
    store: &EventStore,
    workspace_id: Uuid,
    repos: &[WorkspaceRepo],
    cloning: &HashSet<Uuid>,
) -> Vec<CoverageEntry> {
    let maps = store.repo_mappings(workspace_id).unwrap_or_default();
    let aliases = aliases(store, workspace_id, None);
    coverage_from(&maps, &aliases, repos, cloning)
}

/// [`coverage`] of the live repos in `roster`, with the synced aliases this
/// device accepted that the workspace still names.
pub fn coverage_in(
    store: &EventStore,
    workspace_id: Uuid,
    roster: &RosterState,
    cloning: &HashSet<Uuid>,
) -> Vec<CoverageEntry> {
    let maps = store.repo_mappings(workspace_id).unwrap_or_default();
    coverage_from(
        &maps,
        &aliases(store, workspace_id, Some(roster)),
        &live_repos(roster),
        cloning,
    )
}

/// [`coverage`] from mappings already read (runs git; call it without
/// holding the service lock).
pub fn coverage_from(
    maps: &[RepoMapping],
    aliases: &Aliases,
    repos: &[WorkspaceRepo],
    cloning: &HashSet<Uuid>,
) -> Vec<CoverageEntry> {
    let aliases = aliases.clone();
    let mut out = Vec::new();
    for r in repos
        .iter()
        .take(ligus_core::workspace_repo::MAX_COVERAGE_ENTRIES)
    {
        let mine: Vec<&RepoMapping> = maps.iter().filter(|m| m.repo_id == r.id).collect();
        let entry = |state: &str, code: &str, via_fork: bool| CoverageEntry {
            repo_id: r.id,
            state: state.into(),
            code: code.into(),
            via_fork,
        };
        if cloning.contains(&r.id) {
            out.push(entry("cloning", "", false));
            continue;
        }
        let confirmed: Vec<&&RepoMapping> = mine
            .iter()
            .filter(|m| m.confirmed && m.path.exists())
            .collect();
        if let Some(m) = confirmed
            .iter()
            .find(|m| checkout_matches(&m.path, r, &aliases).is_some())
        {
            out.push(entry("present", "", m.via_fork));
        } else if !confirmed.is_empty() {
            out.push(entry("error", "remote_mismatch", false));
        } else if mine.iter().any(|m| !m.confirmed) {
            out.push(entry("missing", "unconfirmed", false));
        } else {
            out.push(entry("missing", "", false));
        }
    }
    out
}

/// One host's coverage as a reader shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostReport {
    pub host_id: String,
    pub label: String,
    pub kind: ligus_core::HostKind,
    pub online: bool,
    /// `None`: the host reports nothing ("unknown", never "missing").
    pub coverage: Option<Vec<CoverageEntry>>,
}

/// Every workspace host with the coverage it last reported, filtered by
/// the **current** roster (review F9a: authz at causal position never
/// retracts, so a revoked device or a removed member's host would keep its
/// last "present" forever otherwise): a host that is a device must be a
/// trusted (certified, unrevoked) one now, and a host's owner must still be
/// a member.
pub fn host_reports(
    roster: &RosterState,
    hosts: &[ligus_core::WorkspaceHost],
    presence: &crate::HostPresence,
) -> Vec<HostReport> {
    let now = ligus_core::Timestamp::now();
    hosts
        .iter()
        .filter(|h| match Uuid::parse_str(&h.id) {
            Ok(d) if roster.devices.contains_key(&d) => roster.device_trusted(d),
            _ => true,
        })
        .filter(|h| h.owner_actor_id.is_empty() || roster.members.contains_key(&h.owner_actor_id))
        .map(|h| HostReport {
            host_id: h.id.clone(),
            label: h.label.clone(),
            kind: h.kind,
            online: presence.is_online(h, &now),
            coverage: presence.coverage_of(&h.id),
        })
        .collect()
}

// ── The turn gate (review F8: the host is the only gate) ─────────────────

/// A checkout ready for a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyCheckout {
    pub repo: WorkspaceRepo,
    pub path: PathBuf,
    pub via_fork: bool,
}

/// Where a chat's turn runs on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnCheckout {
    /// The chat has no repo: today's behaviour (no repo files).
    NoRepo,
    Ready(ReadyCheckout),
    /// Wait: this host is cloning the repo (the turn is already claimed).
    Cloning {
        repo_name: String,
    },
    /// Never run: a structured reason (`code`) and a sentence for the chat.
    Refused {
        code: &'static str,
        reason: String,
    },
}

/// The host-side check before every turn in a chat (repos-and-checkouts
/// 5.3): the chat names a repo; this device has a confirmed mapping (the
/// chat's own override first); the checkout exists, is still that repo by
/// its remote, and isn't managed by another host. Applies to every runtime
/// that touches files, so callers run it in the shared turn preparation.
pub fn turn_checkout(
    store: &EventStore,
    me: Uuid,
    host_label: &str,
    session: &ChatSession,
    roster: &RosterState,
    cloning: &HashSet<Uuid>,
) -> TurnCheckout {
    let Some(repo_id) = session.repo.as_ref().and_then(|r| r.repo_id) else {
        return TurnCheckout::NoRepo;
    };
    let Some(entry) = roster.repos.get(&repo_id) else {
        return TurnCheckout::Refused {
            code: "unknown_repo",
            reason: "this chat's repository isn't known on this host yet (its workspace entry hasn't synced)".into(),
        };
    };
    let name = entry.repo.display_name.clone();
    if entry.removed {
        return TurnCheckout::Refused {
            code: "repo_removed",
            reason: format!("{name} was removed from this workspace"),
        };
    }
    if cloning.contains(&repo_id) {
        return TurnCheckout::Cloning { repo_name: name };
    }
    let path = match store.chat_checkout(session.id, repo_id).ok().flatten() {
        Some(p) => Some((
            p,
            store
                .repo_mappings(session.workspace_id)
                .ok()
                .and_then(|v| {
                    v.into_iter()
                        .find(|m| m.repo_id == repo_id)
                        .map(|m| m.via_fork)
                })
                .unwrap_or(false),
        )),
        None => store
            .primary_checkout(session.workspace_id, repo_id)
            .ok()
            .flatten()
            .map(|m| (m.path, m.via_fork)),
    };
    let Some((path, _)) = path else {
        return TurnCheckout::Refused {
            code: "no_checkout",
            reason: format!(
                "No checkout of {name} on {host_label}. Clone it here or bind a folder."
            ),
        };
    };
    if !crate::git_worktree::is_git_repo(&path) {
        return TurnCheckout::Refused {
            code: "checkout_missing",
            reason: format!("{name}'s checkout on {host_label} is missing"),
        };
    }
    let aliases = aliases(store, session.workspace_id, Some(roster));
    let Some(via) = checkout_matches(&path, &entry.repo, &aliases) else {
        return TurnCheckout::Refused {
            code: "remote_mismatch",
            reason: format!(
                "the checkout of {name} on {host_label} no longer points to {}",
                entry.repo.canonical
            ),
        };
    };
    match marker::claim(&path, me, host_label) {
        Ok(marker::Manager::Me) => {}
        Ok(marker::Manager::Other { label, .. }) => {
            return TurnCheckout::Refused { code: "managed_elsewhere", reason: format!("{name} here is managed by {label}; clone separately to work on it from {host_label}") }
        }
        Err(e) => return TurnCheckout::Refused { code: "checkout_error", reason: format!("{name}: {e}") },
    }
    TurnCheckout::Ready(ReadyCheckout {
        repo: entry.repo.clone(),
        path,
        via_fork: via == MatchVia::Upstream,
    })
}

/// This host's coverage of a chat's repo for the dispatcher (design 11.4),
/// read-only: what [`turn_checkout`] would say, without claiming the
/// checkout's marker. `Present` is what this device signs into its claim.
pub fn dispatch_coverage(
    store: &EventStore,
    me: Uuid,
    session: &ChatSession,
    roster: &RosterState,
    cloning: &HashSet<Uuid>,
) -> ligus_core::dispatch::Coverage {
    use ligus_core::dispatch::Coverage;
    let Some(repo_id) = session.repo.as_ref().and_then(|r| r.repo_id) else {
        return Coverage::NoRepo;
    };
    let Some(entry) = roster.repos.get(&repo_id).filter(|e| !e.removed) else {
        return Coverage::Missing;
    };
    if cloning.contains(&repo_id) {
        return Coverage::Cloning;
    }
    let path = match store.chat_checkout(session.id, repo_id).ok().flatten() {
        Some(p) => Some(p),
        None => store
            .primary_checkout(session.workspace_id, repo_id)
            .ok()
            .flatten()
            .map(|m| m.path),
    };
    let Some(path) = path else {
        return Coverage::Missing;
    };
    if !crate::git_worktree::is_git_repo(&path) {
        return Coverage::Missing;
    }
    let aliases = aliases(store, session.workspace_id, Some(roster));
    if checkout_matches(&path, &entry.repo, &aliases).is_none() {
        return Coverage::Missing;
    }
    match marker::manager_of(&path, me) {
        Some(marker::Manager::Other { .. }) => Coverage::Missing,
        _ => Coverage::Present,
    }
}

/// What a turn's system prompt says about the chat's repo on this host.
pub fn turn_context(c: &TurnCheckout, chat_id: Uuid, base_branch: &str) -> String {
    match c {
        TurnCheckout::NoRepo => String::new(),
        TurnCheckout::Ready(r) => format!(
            "Repository: {} ({}){}, branch {} (the chat's own branch; its base is {}). File changes you make come back as a \
             reviewable proposal applied there.",
            r.repo.display_name,
            r.repo.canonical,
            if r.via_fork { ", through a fork" } else { "" },
            crate::git_worktree::chat_branch_name(&chat_id.to_string()),
            if base_branch.is_empty() { if r.repo.default_branch.is_empty() { "the default branch" } else { &r.repo.default_branch } } else { base_branch },
        ),
        TurnCheckout::Cloning { repo_name } => format!("Repository: {repo_name} is still being cloned on this host."),
        TurnCheckout::Refused { reason, .. } => format!("Repository: {reason}"),
    }
}

// ── The chat branch: pinned base and ledger replay (review F3) ───────────

/// A chat's branch on this host, ready for a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedChat {
    pub chat_worktree: PathBuf,
    /// The chat branch's tip: a proposal's `base_commit`.
    pub tip: String,
    /// The base this host just cut the branch from, when the chat had none
    /// pinned: the caller writes `ChatBasePinned` with it.
    pub pin: Option<String>,
    /// Ledger entries replayed onto the branch now.
    pub replayed: usize,
}

/// Where a chat's branch starts when nothing is pinned: the chat's base
/// branch (or the repo's default) on `upstream` for a fork, else `origin`,
/// then the local branch, then `ligus.chatBase`, then `HEAD`.
pub fn resolve_base(
    checkout: &Path,
    base_branch: &str,
    repo: &WorkspaceRepo,
    via_fork: bool,
) -> Option<String> {
    let b = if base_branch.is_empty() {
        repo.default_branch.as_str()
    } else {
        base_branch
    };
    let mut candidates = Vec::new();
    if !b.is_empty() && ligus_forge::spec::valid_git_ref(b) {
        if via_fork {
            candidates.push(format!("refs/remotes/upstream/{b}"));
        }
        candidates.push(format!("refs/remotes/origin/{b}"));
        candidates.push(format!("refs/heads/{b}"));
    }
    candidates.push(crate::git_worktree::chat_base(checkout));
    candidates.push("HEAD".into());
    candidates.into_iter().find_map(|c| {
        git::git(
            checkout,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{c}^{{commit}}"),
            ],
        )
        .ok()
        .filter(|s| !s.is_empty())
    })
}

/// Get a chat's branch ready on this host: cut it (from the pinned base, or
/// a resolved one to pin) when it doesn't exist, then replay the chat's
/// ledger for the repo. Errors are sentences for the chat ("This chat's
/// branch has diverged on …"); nothing is half-applied.
pub fn prepare_chat(
    ready: &ReadyCheckout,
    session: &ChatSession,
    host_label: &str,
) -> Result<PreparedChat, String> {
    let chat = session.id.to_string();
    let _lock = marker::lock(&ready.path, std::time::Duration::from_secs(30))
        .map_err(|e| format!("{}: {e}", ready.repo.display_name))?;
    let chat_repo = session.repo.clone().unwrap_or_default();
    let mut pin = None;
    if !crate::git_worktree::chat_branch_exists(&ready.path, &chat) {
        let base = match chat_repo.base_commit.as_deref() {
            Some(c) if git::has_commit(&ready.path, c) => c.to_string(),
            Some(c) => {
                return Err(format!(
                    "This chat's base commit {} isn't in {}'s checkout on {host_label}; fetch it there, then ask again",
                    &c[..c.len().min(12)],
                    ready.repo.display_name
                ))
            }
            None => {
                let b = resolve_base(&ready.path, &chat_repo.base_branch, &ready.repo, ready.via_fork)
                    .ok_or_else(|| format!("{} on {host_label} has no commit to start this chat from", ready.repo.display_name))?;
                pin = Some(b.clone());
                b
            }
        };
        crate::git_worktree::chat_worktree_at(&ready.path, &chat, &base)
            .map_err(|e| e.to_string())?;
    } else if let Some(c) = chat_repo.base_commit.as_deref() {
        // The branch exists here: it must have been cut from the chat's
        // pinned base. Another host pinning first (or a member re-pinning)
        // leaves this host's branch on a different base than every other
        // device's, and replaying the ledger onto it would diverge silently
        // (review F3a): refuse instead.
        let branch = format!(
            "refs/heads/{}",
            crate::git_worktree::chat_branch_name(&chat)
        );
        let on = |base: &str| {
            git::has_commit(&ready.path, base)
                && git::git(&ready.path, &["merge-base", "--is-ancestor", base, &branch]).is_ok()
        };
        // Cut from one of the chat's earlier pins: the chat was re-pinned
        // (`ChatBaseRepinned`, review residual 3). Follow it by rebuilding
        // the branch on the new pin from the ledger, or refuse visibly.
        // The commit the branch was cut from (below its run of Ligus
        // commits) when that is an earlier pin; else, for a branch not on
        // the pin in force, the latest earlier pin it does contain (work of
        // someone's own sits on top: the follow refuses and says so).
        let history: &[String] = session
            .repo_bases
            .get(&ready.repo.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let earlier = apply::branch_cut_point(&ready.path, &chat)
            .filter(|cut| cut != c && history.iter().any(|p| p == cut))
            .or_else(|| {
                (!on(c))
                    .then(|| {
                        history
                            .iter()
                            .rev()
                            .find(|p| p.as_str() != c && on(p))
                            .cloned()
                    })
                    .flatten()
            });
        if let Some(old) = earlier {
            if !git::has_commit(&ready.path, c) {
                return Err(format!(
                    "This chat's base was re-pinned to {}, which isn't in {}'s checkout on {host_label}; fetch it there, then ask again",
                    &c[..c.len().min(12)],
                    ready.repo.display_name
                ));
            }
            let entries = ligus_core::workspace_repo::ledger(session, ready.repo.id);
            apply::rebase_onto_repin(&ready.path, &chat, &old, c, &entries).map_err(|e| {
                format!(
                    "This chat's base was re-pinned to {}, and its branch on {host_label} can't follow: {e}. Nothing changed; resolve it there, or remove this host's chat branch to cut it again from the new base.",
                    &c[..c.len().min(12)]
                )
            })?;
        }
        if !on(c) {
            return Err(format!(
                "This chat's branch on {host_label} wasn't cut from the chat's pinned base {}; it has diverged from the other devices. \
                 Start a new chat, or remove this host's chat branch to cut it again from the pinned base.",
                &c[..c.len().min(12)]
            ));
        }
    }
    let wt = crate::git_worktree::chat_worktree(&ready.path, &chat).map_err(|e| e.to_string())?;
    let entries = ligus_core::workspace_repo::ledger(session, ready.repo.id);
    let base = chat_repo
        .base_commit
        .as_deref()
        .filter(|c| git::has_commit(&ready.path, c));
    let replayed = apply::replay_ledger(&ready.path, &chat, &entries, base).map_err(|e| {
        format!("This chat's branch has diverged on {host_label}: {e}. Ask the agent to rebase, or mark the proposal stale.")
    })?;
    let tip = git::git(
        &ready.path,
        &[
            "rev-parse",
            &format!(
                "refs/heads/{}",
                crate::git_worktree::chat_branch_name(&chat)
            ),
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(PreparedChat {
        chat_worktree: wt,
        tip,
        pin,
        replayed,
    })
}

#[cfg(test)]
mod tests;

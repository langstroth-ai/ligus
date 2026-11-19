//! Workspace repositories (the repos-and-checkouts design; the wire is frozen
//! in 0.1, the UI and dispatch use come later).
//!
//! * A **workspace repo** is the workspace's signed statement "we work on this
//!   code" (config log: `WorkspaceRepoAdded`, `WorkspaceRepoUpdated`,
//!   `WorkspaceRepoRemoved`). It is not a path, a URL with credentials, a
//!   clone instruction or a trust grant. Its id is
//!   `UUIDv5(LIGUS_REPO_NS, canonical)`, so two Admins adding the same repo get
//!   the same id. The fold refuses a `canonical` that isn't already in normal
//!   form ([`canonical_is_normal`]) or an id that doesn't match it.
//! * A **chat repo** (`ChatRepoSet`, chat log) picks zero or one workspace repo
//!   for a chat. A proposal's `repo_id` is derived from it when the proposal is
//!   created and never changes afterwards (repos review F1).
//! * A **chat base** (`ChatBasePinned`, chat log) pins the commit the chat's
//!   branch was cut from, so every device replays applied proposals onto the
//!   same base (review F3).
//! * **Coverage** ("host H has repo R checked out") is advisory and rides the
//!   relay's sealed presence beat ([`CoverageEntry`]), never the log: the host
//!   that runs a turn re-checks its own checkout and is the only gate (review
//!   F8, F9).
//!
//! Checkout paths, scan roots and clone jobs are device-local and never appear
//! in any of these types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The namespace of workspace repo ids (a fixed UUID, not a signature domain
/// string). Never change it: ids derived from it are on the log.
pub const LIGUS_REPO_NS: Uuid = Uuid::from_u128(0x6c69_6775_732d_7265_706f_2d6e_7376_3031);

/// At most this many live repos per workspace.
pub const MAX_LIVE_REPOS: usize = 64;
/// Longest display name (characters).
pub const MAX_DISPLAY_NAME: usize = 80;
/// At most this many path segments in a forge canonical.
pub const MAX_SEGMENTS: usize = 20;
/// At most this many coverage entries in one beat.
pub const MAX_COVERAGE_ENTRIES: usize = 64;

/// Hosts whose owner/name paths are case-insensitive: their canonical path is
/// lowercase. A fixed table (review F5), never a device's own forge settings.
pub const CASE_INSENSITIVE_HOSTS: &[&str] = &["github.com", "gitlab.com", "codeberg.org", "gitea.com"];
/// Forge kinds whose paths are case-insensitive (the repo entry's own hint).
pub const CASE_INSENSITIVE_KINDS: &[&str] = &["github", "gitlab", "gitea", "forgejo"];

/// A repository the workspace works on (config log).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRepo {
    /// `UUIDv5(LIGUS_REPO_NS, canonical)` ([`repo_id`]).
    pub id: Uuid,
    /// `host/owner/name` in normal form, or `local:<root commit>` for a repo
    /// with no usable remote.
    pub canonical: String,
    /// "WeFit": at most 80 characters, no control characters.
    pub display_name: String,
    /// "github" | "gitlab" | "gitea" | "forgejo" | "" (unknown): a display
    /// hint, and the case rule for hosts outside the fixed table.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub forge_kind: String,
    /// A hint; the forge (or the checkout) is authoritative.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_branch: String,
    /// The history's root commit (40 or 64 lowercase hex), for a membership
    /// check on a checkout. Unverified until a device checks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_commit: Option<String>,
}

/// The repo id for a canonical.
pub fn repo_id(canonical: &str) -> Uuid {
    Uuid::new_v5(&LIGUS_REPO_NS, canonical.as_bytes())
}

/// Whether `s` is a commit id: 40 (SHA-1) or 64 (SHA-256) lowercase hex.
pub fn is_commit_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && !h.starts_with("www.")
        && h.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `canonical` is already in normal form (review F5): `local:<root>`,
/// or `host/seg/…` with a lowercase ASCII host (no IDNA, no `www.`, no port,
/// no userinfo), 2 to 20 strict segments, no trailing `.git`, and a lowercase
/// path for a case-insensitive host or forge kind. The fold refuses anything
/// else, so two devices can never hold two spellings of one repo.
pub fn canonical_is_normal(canonical: &str, forge_kind: &str) -> bool {
    if let Some(root) = canonical.strip_prefix("local:") {
        return is_commit_id(root);
    }
    let Some((host, path)) = canonical.split_once('/') else { return false };
    if !valid_host(host) {
        return false;
    }
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() < 2 || segs.len() > MAX_SEGMENTS || !segs.iter().all(|s| valid_segment(s)) {
        return false;
    }
    if segs.last().is_some_and(|s| s.ends_with(".git")) {
        return false;
    }
    let folds = CASE_INSENSITIVE_HOSTS.contains(&host) || CASE_INSENSITIVE_KINDS.contains(&forge_kind);
    !(folds && path.bytes().any(|b| b.is_ascii_uppercase()))
}

/// Whether a display name is acceptable: non-empty, at most 80 characters, no
/// control or bidi-override characters.
pub fn display_name_ok(name: &str) -> bool {
    let n = name.chars().count();
    n > 0
        && n <= MAX_DISPLAY_NAME
        && !name.chars().any(|c| c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}'))
}

/// The checks a `WorkspaceRepoAdded` payload must pass on its own (the fold
/// adds the role, presence, liveness and count rules).
pub fn repo_well_formed(repo: &WorkspaceRepo) -> Result<(), &'static str> {
    if !canonical_is_normal(&repo.canonical, &repo.forge_kind) {
        return Err("the repo's canonical name isn't in normal form");
    }
    if repo.id != repo_id(&repo.canonical) {
        return Err("the repo id doesn't match its canonical name");
    }
    if !display_name_ok(&repo.display_name) {
        return Err("bad repo display name");
    }
    if repo.forge_kind.len() > 32 || repo.default_branch.len() > 255 || repo.default_branch.chars().any(char::is_control) {
        return Err("bad repo hint");
    }
    if repo.root_commit.as_deref().is_some_and(|r| !is_commit_id(r)) {
        return Err("bad root commit");
    }
    Ok(())
}

/// One workspace repo as the config roster holds it. A removed repo stays as
/// a tombstone until a later add (same id) brings it back, so chats that name
/// it keep showing "removed repository".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoEntry {
    pub repo: WorkspaceRepo,
    pub removed: bool,
}

/// How many bases one chat may pin for one repo (the first pin and its
/// re-pins): a bound on the fold's history, far above any real chat's.
pub const MAX_BASE_PINS: usize = 64;

/// A chat's repository (`ChatRepoSet`), folded onto the chat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRepo {
    /// `None` = this chat has no repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<Uuid>,
    /// "" = the repo's default branch.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_branch: String,
    /// The commit the chat's branch was cut from (`ChatBasePinned`), when one
    /// is pinned for this repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
}

/// A human's request that a specific host apply an approved proposal
/// (`ProposalApplyRequested`; the phone's "Apply on <host>", separate from
/// approving). The host fulfils it with a `ProposalApplied` naming
/// `request_id`; the fold accepts that apply only from the named host, for
/// the named digest, onto the named base.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRequest {
    pub id: Uuid,
    pub proposal_id: Uuid,
    /// The approved content the person saw (`authz::votes::diff_digest`).
    pub diff_digest: String,
    /// The device asked to apply it.
    pub host_device_id: Uuid,
    /// The base the person saw (40/64 hex): the host refuses (and the fold
    /// denies the apply) on any other base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_base_commit: Option<String>,
    /// The proposal's repo, when it has one (must match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<Uuid>,
}

/// The longest failure reason a host writes (`ProposalApplyFailed`).
pub const MAX_APPLY_FAILURE_CHARS: usize = 500;

/// How a closed apply request ended (folded from `ProposalApplied`,
/// `ProposalApplyFailed` and `ProposalApplyWithdrawn`; the first in canonical
/// order wins, and the fold allows only one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ApplyOutcome {
    /// The named host applied it (`commit`, when it made one).
    Applied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
    },
    /// The named host couldn't, and said why.
    Failed { reason: String },
    /// Its requester (or an Admin) withdrew it.
    Withdrawn,
}

/// One host's coverage of one workspace repo, as it rides the host's sealed
/// presence beat (`host_presence`; never the log). Advisory: dispatch may
/// show it, but the host that runs a turn re-checks its own checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageEntry {
    pub repo_id: Uuid,
    /// A string code, so a newer state reads as unknown instead of failing the
    /// beat: "present" | "cloning" | "missing" | "error" (anything else is
    /// unknown).
    pub state: String,
    /// A short machine code for "error" (`clone_failed`, `remote_mismatch`…).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// The checkout covers the repo through a fork (`upstream` remote). The
    /// fork's own name is never synced.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub via_fork: bool,
}

/// A coverage state as a reader sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageState {
    Present,
    Cloning,
    Missing,
    Error,
    Unknown,
}

impl CoverageEntry {
    pub fn state(&self) -> CoverageState {
        match self.state.as_str() {
            "present" => CoverageState::Present,
            "cloning" => CoverageState::Cloning,
            "missing" => CoverageState::Missing,
            "error" => CoverageState::Error,
            _ => CoverageState::Unknown,
        }
    }
}

/// A host's liveness beat (the relay presence slot `host:<host_id>`, sealed
/// under the workspace key). `seq` orders beats from one host when they
/// arrive out of order (highest wins, ties by `at`); `coverage` replaces the
/// host's previous coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostBeat {
    pub host_id: String,
    /// Unix seconds.
    pub at: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage: Vec<CoverageEntry>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl HostBeat {
    /// Bounds a reader enforces before using a beat's coverage.
    pub fn coverage_ok(&self) -> bool {
        self.coverage.len() <= MAX_COVERAGE_ENTRIES && self.coverage.iter().all(|c| c.state.len() <= 16 && c.code.len() <= 64)
    }
}


// ── Canonical identity (repos review F5) ─────────────────────────────────

/// Why a remote has no canonical identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonError {
    /// A local path, a `file://` URL or no remote at all: the repo can only
    /// be matched by its root commit (`local:<root>`).
    NoRemote,
    /// A transport or character Ligus never treats as an identity (`ext::`,
    /// non-ASCII hosts, encoded separators, spaces, `..`).
    Refused(&'static str),
}

impl std::fmt::Display for CanonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanonError::NoRemote => f.write_str("no forge remote"),
            CanonError::Refused(why) => f.write_str(why),
        }
    }
}

/// Whether `forge_kind` (a repo entry's hint) or the host's place in the
/// fixed table makes owner/name lookups case-insensitive.
pub fn folds_case(host: &str, forge_kind: &str) -> bool {
    CASE_INSENSITIVE_HOSTS.contains(&host) || CASE_INSENSITIVE_KINDS.contains(&forge_kind)
}

/// The canonical identity (`host/owner/name`) of a git remote URL.
///
/// A pure function of its inputs (review F5): the remote string as git
/// config stores it (never after `insteadOf`), the case hint of the repo
/// entry it is compared with (`forge_kind`, "" for none), and this device's
/// own SSH host aliases (`github-work` → `github.com`; device-local, never
/// synced). No DNS, no IDNA, no forge settings, so every device computing it
/// from the same inputs gets the same string, and the result always passes
/// [`canonical_is_normal`].
///
/// Accepted: `https://`, `http://`, `ssh://`, `git://`, `git+ssh://` URLs and
/// scp-style `[user@]host:path`. Userinfo, port, a trailing `.git` and
/// trailing slashes are dropped; the host is lowercased, `www.` and a
/// trailing dot are stripped and `ssh.github.com` is `github.com`. Refused:
/// non-ASCII hosts, IPv6 literals, `%`, `\`, `?`, `#`, spaces, control
/// characters, `.`/`..` segments, more than 20 segments, and every other
/// transport (`ext::`, `fd::`, `file://` and bare paths give [`CanonError::NoRemote`]).
pub fn canonical_repo(
    remote: &str,
    forge_kind: &str,
    ssh_aliases: &std::collections::BTreeMap<String, String>,
) -> Result<String, CanonError> {
    let url = remote.trim();
    if url.is_empty() {
        return Err(CanonError::NoRemote);
    }
    if url.len() > 1024 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(CanonError::Refused("spaces or control characters in a remote"));
    }
    if !url.is_ascii() {
        return Err(CanonError::Refused("a non-ASCII remote (write the host in punycode)"));
    }
    if url.contains("::") {
        return Err(CanonError::Refused("a git remote-helper transport is never an identity"));
    }
    let (authority, path, scp) = if let Some((scheme, rest)) = url.split_once("://") {
        match scheme.to_ascii_lowercase().as_str() {
            "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git" => {}
            "file" => return Err(CanonError::NoRemote),
            _ => return Err(CanonError::Refused("an unknown transport")),
        }
        let (auth, path) = rest.split_once('/').ok_or(CanonError::Refused("a remote without a path"))?;
        (auth, path, false)
    } else if let Some((left, path)) = url.split_once(':').filter(|(l, _)| !l.contains('/') && l.len() > 1) {
        (left, path, true)
    } else {
        return Err(CanonError::NoRemote);
    };
    // In the authority too, not only the path: git (curl) ends the host at
    // `#` or `?`, so in `https://evil.example#@github.com/o/n` it connects
    // to evil.example while the text after `@` reads as github.com.
    if authority.contains(['#', '?', '\\']) {
        return Err(CanonError::Refused("a reserved character before the host"));
    }
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    if host_port.starts_with('[') {
        return Err(CanonError::Refused("an IP literal host"));
    }
    let host = if scp { host_port } else { host_port.split(':').next().unwrap_or(host_port) };
    let mut host = host.to_ascii_lowercase();
    while host.ends_with('.') {
        host.pop();
    }
    if let Some(h) = host.strip_prefix("www.") {
        host = h.to_string();
    }
    if host == "ssh.github.com" {
        host = "github.com".into();
    }
    if let Some(mapped) = ssh_aliases.get(&host) {
        host = mapped.to_ascii_lowercase();
    }
    if !valid_host(&host) {
        return Err(CanonError::Refused("not a valid host name"));
    }
    if path.contains(['%', '\\', '?', '#']) {
        return Err(CanonError::Refused("an encoded or reserved character in the path"));
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() < 2 || segs.len() > MAX_SEGMENTS || !segs.iter().all(|s| valid_segment(s)) {
        return Err(CanonError::Refused("not an owner/name path"));
    }
    let path = if folds_case(&host, forge_kind) { path.to_ascii_lowercase() } else { path.to_string() };
    let canonical = format!("{host}/{path}");
    if canonical.len() > 300 || !canonical_is_normal(&canonical, forge_kind) {
        return Err(CanonError::Refused("not a normal-form repository name"));
    }
    Ok(canonical)
}

/// The host and path of a forge canonical (`None` for `local:`).
pub fn split_canonical(canonical: &str) -> Option<(&str, &str)> {
    if canonical.starts_with("local:") {
        return None;
    }
    canonical.split_once('/')
}

/// Look-alike flags (repos-and-checkouts 7): each live repo that resembles
/// an earlier-added live repo with a different canonical, mapped to that
/// repo. Two signs, either is enough: the same claimed root commit ("same
/// history as WeFit, different owner"; the claim is the Admin's, unverified
/// until a device checks it), or a name that reads the same once common
/// ASCII confusables are folded (`zazu1am` / `zazulam`, `rn` / `m`, `we-fit`
/// / `wefit`, case), which needs no claim at all, so leaving the root out
/// doesn't evade it. A display warning, never a refusal.
pub fn lookalikes(repos: &[WorkspaceRepo]) -> std::collections::BTreeMap<Uuid, Uuid> {
    let mut out = std::collections::BTreeMap::new();
    let skeletons: Vec<String> = repos.iter().map(|r| confusable_skeleton(&r.canonical)).collect();
    for (i, r) in repos.iter().enumerate() {
        let root = r.root_commit.as_deref();
        if let Some(first) = repos[..i].iter().enumerate().find(|(j, o)| {
            o.canonical != r.canonical && ((root.is_some() && o.root_commit.as_deref() == root) || skeletons[*j] == skeletons[i])
        }) {
            out.insert(r.id, first.1.id);
        }
    }
    out
}

/// A canonical with common ASCII confusables folded (case; `0`→o, `1`/`i`→l,
/// `5`→s, `8`→b; `rn`→m, `vv`→w; `-`, `_`, `.` dropped; `/` kept).
fn confusable_skeleton(canonical: &str) -> String {
    if canonical.starts_with("local:") {
        // A root commit id: hex, compared exactly.
        return canonical.to_string();
    }
    let folded: String = canonical
        .chars()
        .filter_map(|c| match c.to_ascii_lowercase() {
            '0' => Some('o'),
            '1' | 'i' | '|' => Some('l'),
            '5' => Some('s'),
            '8' => Some('b'),
            '-' | '_' | '.' => None,
            c => Some(c),
        })
        .collect();
    folded.replace("rn", "m").replace("vv", "w")
}

// ── The chat ledger (repos review F3) ────────────────────────────────────

/// One applied proposal in a chat's ledger for a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub proposal_id: Uuid,
    /// The exactly-approved patch.
    pub diff: String,
    /// Reverted after it was applied (`ProposalReverted`): a replay takes it
    /// back out.
    pub reverted: bool,
}

/// The commit trailer naming the proposal a Ligus apply commit carries.
pub const PROPOSAL_TRAILER: &str = "Ligus-Proposal";
/// The commit trailer naming the proposal a Ligus revert commit takes out.
pub const REVERT_TRAILER: &str = "Ligus-Revert";

/// A chat's ledger for `repo_id`: its file-diff proposals for that repo in
/// the order the fold first saw each applied (`ChatSession::applied_order`,
/// so a proposal applied on two devices at once appears once), each with
/// the approved bytes and whether it was reverted since.
pub fn ledger(session: &crate::session::ChatSession, repo_id: Uuid) -> Vec<LedgerEntry> {
    session
        .applied_order
        .iter()
        .filter_map(|id| session.proposals.iter().find(|p| p.id == *id))
        .filter(|p| p.repo_id == Some(repo_id) && p.kind == crate::proposals::ProposalKind::FileDiff)
        .filter_map(|p| p.diff.as_ref().map(|d| LedgerEntry { proposal_id: p.id, diff: d.clone(), reverted: p.reverted }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_normal_form() {
        for ok in [
            "github.com/zazulam/wefit",
            "git.example.com/Org/Repo",
            "gitlab.example.com/group/sub/proj",
            "local:0123456789abcdef0123456789abcdef01234567",
        ] {
            assert!(canonical_is_normal(ok, ""), "{ok}");
        }
        for bad in [
            "github.com/ZaZulam/WeFit",
            "GitHub.com/zazulam/wefit",
            "www.github.com/zazulam/wefit",
            "github.com/zazulam/wefit.git",
            "github.com/zazulam",
            "github.com/zazu lam/wefit",
            "github.com/zazulam/../wefit",
            "github.com/zazulam/we%2Ffit",
            "github.com:22/zazulam/wefit",
            "bücher.example/o/r",
            "local:XYZ",
            "local:0123",
            "",
        ] {
            assert!(!canonical_is_normal(bad, ""), "{bad}");
        }
        // An unknown forge keeps case, unless the entry says it folds.
        assert!(!canonical_is_normal("git.example.com/Org/Repo", "gitea"));
    }

    #[test]
    fn repo_ids_are_derived() {
        let c = "github.com/zazulam/wefit";
        let r = WorkspaceRepo {
            id: repo_id(c),
            canonical: c.into(),
            display_name: "WeFit".into(),
            forge_kind: "github".into(),
            default_branch: "main".into(),
            root_commit: None,
        };
        assert!(repo_well_formed(&r).is_ok());
        assert!(repo_well_formed(&WorkspaceRepo { id: Uuid::nil(), ..r.clone() }).is_err());
        assert!(repo_well_formed(&WorkspaceRepo { display_name: "We\u{202E}Fit".into(), ..r.clone() }).is_err());
        assert!(repo_well_formed(&WorkspaceRepo { root_commit: Some("abc".into()), ..r }).is_err());
    }

    /// The beat's wire bytes: a coverage-less beat is exactly the Hive 1.x/Ligus 0.1
    /// beat; coverage adds `seq` and `coverage` (frozen).
    #[test]
    fn host_beat_wire_is_frozen() {
        let plain = HostBeat { host_id: "h".into(), at: 1, seq: 0, coverage: vec![] };
        assert_eq!(serde_json::to_string(&plain).unwrap(), r#"{"hostId":"h","at":1}"#);
        let full = HostBeat {
            host_id: "h".into(),
            at: 1,
            seq: 7,
            coverage: vec![CoverageEntry { repo_id: Uuid::nil(), state: "error".into(), code: "clone_failed".into(), via_fork: true }],
        };
        assert_eq!(
            serde_json::to_string(&full).unwrap(),
            r#"{"hostId":"h","at":1,"seq":7,"coverage":[{"repoId":"00000000-0000-0000-0000-000000000000","state":"error","code":"clone_failed","viaFork":true}]}"#
        );
        assert!(full.coverage_ok());
    }

    /// git (curl) ends a URL's host at `#` or `?`, so
    /// `https://evil.example#@github.com/o/n` fetches from evil.example. Its
    /// identity must never read as github.com's repo (adversarial review).
    #[test]
    fn canonical_refuses_a_host_git_would_not_connect_to() {
        let none = std::collections::BTreeMap::new();
        for url in [
            "https://evil.example#@github.com/zazulam/wefit",
            "https://evil.example?@github.com/zazulam/wefit",
            "https://evil.example\\@github.com/zazulam/wefit",
            "ssh://evil.example#@github.com/zazulam/wefit",
            "evil.example#@github.com:zazulam/wefit",
        ] {
            let c = canonical_repo(url, "", &none);
            assert!(c.as_deref() != Ok("github.com/zazulam/wefit"), "{url} → {c:?}");
        }
        assert_eq!(canonical_repo("https://user@github.com/zazulam/wefit", "", &none).as_deref(), Ok("github.com/zazulam/wefit"));
    }

    /// A look-alike is flagged without any root-commit claim: the Admin who
    /// adds `zazu1am/wefit` next to `zazulam/wefit` simply omits the root.
    #[test]
    fn lookalike_flagged_by_confusable_name_without_a_root_claim() {
        let mk = |c: &str| WorkspaceRepo {
            id: repo_id(c),
            canonical: c.into(),
            display_name: "WeFit".into(),
            forge_kind: String::new(),
            default_branch: String::new(),
            root_commit: None,
        };
        let a = mk("github.com/zazulam/wefit");
        for evil in ["github.com/zazu1am/wefit", "github.com/zazuiam/wefit", "github.com/zazularn/wefit", "github.com/zazulam/wef1t", "githu8.com/zazulam/wefit", "github.com/zazulam/we-fit"] {
            let b = mk(evil);
            let flags = lookalikes(&[a.clone(), b.clone()]);
            assert_eq!(flags.get(&b.id), Some(&a.id), "{evil}");
        }
        let c = mk("github.com/zazulam/infra");
        assert!(lookalikes(&[a.clone(), c]).is_empty(), "different names aren't look-alikes");
    }

    #[test]
    fn lookalike_flagged_by_fingerprint() {
        let mk = |c: &str, root: Option<&str>| WorkspaceRepo {
            id: repo_id(c),
            canonical: c.into(),
            display_name: c.into(),
            forge_kind: String::new(),
            default_branch: String::new(),
            root_commit: root.map(str::to_owned),
        };
        let root = "0123456789abcdef0123456789abcdef01234567";
        let a = mk("github.com/zazulam/wefit", Some(root));
        let b = mk("github.com/zazu1am/wefit", Some(root));
        let c = mk("github.com/zazulam/infra", None);
        let flags = lookalikes(&[a.clone(), b.clone(), c]);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags.get(&b.id), Some(&a.id));
    }

    #[test]
    fn unknown_coverage_state_reads_as_unknown() {
        let e: CoverageEntry = serde_json::from_str(r#"{"repoId":"00000000-0000-0000-0000-000000000000","state":"quantum"}"#).unwrap();
        assert_eq!(e.state(), CoverageState::Unknown);
    }
}

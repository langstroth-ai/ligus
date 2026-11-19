# Ligus 2.0+: workspace repositories, per-device checkouts, and where turns run

Status: **implemented in Ligus 0.1.0 with the review's required changes**
(see [the review](repos-and-checkouts.review.md); user docs:
`docs-site/docs/ligus/features/repositories.md`). Where this design and the
review differ, the review's required changes won: no digest v2 (the sticky
repo and the digest parts already bind it), coverage on the presence beat
rather than the log, the host is the only turn gate. Workspace-synced
aliases, first dropped (SSH host aliases are device-local), came back in
section 12 with the review's R2 rules, and the device-keyed dispatcher the
review sequenced Phase 2 after is section 11. What is left is listed in the
implementation notes at the end. Written 2026-11-16 against
`release/v2.0.0` (Ligus is short for *ligustica*, the Italian honeybee).
Citations are `file:line` or `file` + function name; line numbers drift, and
when this document and the code disagree, the code wins.

Related: [phase0-specs.md](phase0-specs.md) (authz at causal position, rules
v2 presence, workspace trust, telemetry split),
[identity-and-forges.md](identity-and-forges.md) (the `Forge` trait, token
locality, internal hosts), [architecture-review.md](architecture-review.md)
(dispatch invariant 4.9, TurnBundle 3.3),
[directory-trust.md](directory-trust.md).

---

## 0. Summary

Today a workspace has no idea which code it is about. Each device keeps its
own private answer: the desktop stores a folder per workspace in
`settings.json`, the serve box stores a checkout name per workspace and per
chat in `repo-bindings.toml`, and neither tells anyone else. So a phone
talking to a box sees nothing, even when the desktop on the same Mac has the
repo open.

The recommendation (option A, section 9) splits the problem in two:

| | What it is | Where it lives | Synced? |
|---|---|---|---|
| **Workspace repo** | "This workspace works on `github.com/zazulam/wefit`, default branch `main`, display name WeFit." A canonical remote identity and a stable id. | Signed event on the workspace config log. Admin to add or remove. | Yes |
| **Checkout mapping** | "On this device, repo `R` is at `/Users/zaz/apps/WeFit`." | Device-local table in the data dir. | **Never** |
| **Coverage** | "Host `H` has a checkout of `R` (yes / no / cloning), head commit." No paths. | Small signed record by the host itself. | Yes |
| **Chat repo** | "This chat works in repo `R`, on branch `main`." | Session event on the chat log. | Yes |

With these, the box can look up "which repos does this workspace have",
find or clone a matching checkout, and every client (including a phone that
only reaches one box) can show which hosts can run a turn on which repo.
Dispatch checks coverage before it sends a turn to a pinned host, and
proposals carry the repo id and base commit so any device with a checkout
can apply them.

The MVP (Phase 1, section 9.4) fixes the owner's case: the desktop proposes
a workspace repo from the bound folder's remote, the box sees it, matches or
clones it, and the phone shows coverage and a "Clone here" button.

---

## 1. Problem

### 1.1 The concrete failure

The owner bound `~/apps/WeFit` to a workspace in the desktop app. Their
`ligus serve` box runs on the same Mac, enrolled as their own device. The
phone, paired with the box, shows "No checkouts on the host yet." in the
repo picker and "No repository yet." for the chat
(`ligus-ios/Hive/Views/RepoPickerView.swift:366` and `:277`).

Why, step by step:

1. The desktop's "Choose folder…" (`web/src/components/BindWorkspacePrompt.tsx`)
   calls `set_workspace_root` (`app/src/lib.rs:10969`). That writes the path
   into `LiveSettings.workspace_dirs[<active workspace id>]`
   (`app/src/lib.rs:430-439`), persisted in `<desktop data dir>/settings.json`.
   The doc comment says it plainly: device-local, never synced.
2. Nothing is written to any log. There is no repo-related config event at all
   (the closest is `HomeBeacon.base_branch`, `crates/hive-core/src/flow/mod.rs:238`,
   which is a branch name with no repo attached).
3. The box is a separate process with its own data dir and its own device id.
   Its checkouts are whatever sits directly under `repos_dir`
   (`crates/hive-cli/src/repos.rs:43`, default `<box data>/repos`) and contains
   `.git` (`local_checkouts`, `crates/hive-cli/src/serve/repos_api.rs:558`). Its
   bindings are `<box data>/repo-bindings.toml` (`RepoBindings`,
   `repos.rs:33`), which is empty.
4. `GET /v1/repos` therefore answers `local: []`, and `GET /v1/chats/:id/repo`
   answers `{repo: null, ...}` (`chat_binding`, `repos_api.rs:586`). The agent's
   system prompt on the box says "Repository: none is bound to this chat on
   this host" (`repos::turn_context`, `repos.rs:470`).

Being on the same disk doesn't help: the box never looks outside `repos_dir`
unless someone runs `ligus bind-repo /Users/zaz/apps/WeFit` on it
(`cmd_bind_repo`, `repos.rs:512`, which accepts an absolute path for the
workspace default). That works as a workaround today, but nothing tells the
owner to do it, and doing it creates a second hazard (section 4.5): two
processes driving `.ligus/worktrees/chat-<id>` in the same repository.

### 1.2 The general cases

| # | Situation | What happens today |
|---|---|---|
| G1 | Several devices with different checkouts of the same code (laptop at `~/src/wefit`, desktop at `D:\code\WeFit`, box at `<data>/repos/zazulam__wefit`). | Each device binds independently. Nobody can tell they are the same repo. Proposals carry a bare diff (`ActionProposal.diff`, `crates/hive-core/src/proposals.rs:95-100`) and "any device whose tree matches the base can apply it", but nothing says which tree. |
| G2 | An agent pinned to a host (`WorkspaceAgent.host_id`, `crates/hive-core/src/agent.rs:34`) that has a different checkout, or none. | Dispatch (`turn_runs_here`, `app/src/lib.rs:6921`; `flow::candidates`, `app/src/flow.rs:255-270`; `workflows::prepare_stage`) checks host binding and online state only. A turn reaches a box with no checkout and runs without file tools or in a scratch dir. The user finds out from the agent's reply. |
| G3 | People pointing at forks, personal clones, or different branches. | The box's fork layout (`ForkLayout`, `check_push_target`, `repos.rs:229-376`) handles one person's fork on one box. Two members whose `origin` is their own fork of the same upstream look like two unrelated repos. |
| G4 | Several repos in one workspace (app + API + infra). | Desktop: one folder per workspace, no per-chat binding. Box: one workspace default plus per-chat overrides, by local checkout name. No shared list. |
| G5 | A phone that only sees one box. | The phone talks to one `HostProfile` at a time (`ligus-ios/Hive/App/HostProfiles.swift:25`, `AppModel.client`). It sees that box's checkouts and nothing about the owner's desktop, so it can't say "WeFit is on your Mac, not on this box". |
| G6 | Two Ligus processes on one machine, same OS user (owner's case). | Both may end up using the same checkout. Both create `hive/chat/<id>` worktrees at `<repo>/.ligus/worktrees/chat-<slug>` (`git_worktree.rs:120-125`). Git refuses to check out one branch in two worktrees, so the second one fails or, worse, falls back to running in place (`apply_chat_dir`, `app/src/lib.rs:6730`). |

### 1.3 What exists today (inventory)

| Piece | Where | Shape |
|---|---|---|
| Desktop binding | `LiveSettings.workspace_dirs` (`app/src/lib.rs:439`), `settings.json` | workspace id → folder. One per workspace. No unbind command (no `workspace_dirs.remove` anywhere). Legacy `workspace_root` migrated by `migrate_local_workspace_dir` (`:1390`). |
| Desktop root adoption | `is_auto_adoptable_workspace` (`:1330`), `initial_workspace_root` (`:1344`) | cwd adopted only with `.git` or `.ligus/`, never shallow or system dirs (the v1.10.2 launch-hang fix). |
| Desktop turn dir | `run_prepared_turn_inner` (`:8395`), `turn_repo_root` (`:8072`), `isolates_turn` / `isolates_on_write` | Per-turn worktree cut from the chat branch; refuses when the envelope says `isolate_writes` and no worktree could be made (`:8603-8608`). |
| Box binding | `RepoBindings` (`repos.rs:33`), `repo-bindings.toml` | `workspace = "<name or abs path>"`, `[chats] "<uuid>" = "<name>"`. Resolution chat → workspace (`resolve`, `:80`). |
| Box repo API | `repos_api.rs` | `GET /v1/repos`, `POST /v1/repos/clone`, `GET/PUT /v1/chats/:id/repo`, `pull`, `pr`; `forges_api.rs` for connections. Clone spec checked by `hive_forge::spec::parse_repo_spec` and must name a connected host (`resolve_spec`, `:813`). Paths never leave the box (`binding_name`, `:575`). |
| Worktrees | `crates/hive-runtime/src/git_worktree.rs` | `<repo>/.ligus/worktrees/chat-<slug>` on branch `hive/chat/<slug>` (note: the `hive/` prefix is still used in 2.0); turn trees `hive/turn-<slug>`; pool; `chat_base` reads `hive.chatBase`, else `HEAD`. |
| Hygiene | `crates/hive-runtime/src/hygiene.rs`, driver `run_hygiene_sweep` (`app/src/guards.rs:572`) | Sweeps stale trees; marks open diff proposals stale when `git apply --check` fails against the chat checkout. Runs only on the active workspace's bound repo. |
| Repo-supplied config trust | `crates/hive-runtime/src/workspace_trust.rs` | Repo runtimes and MCP servers load inert; per-item digest enablement in device-local `workspace_trust`; `root_id = SHA-256(canonical path ‖ first commit)` (`:150`). |
| Proposals | `ActionProposal` (`proposals.rs:70`) | Diff + changed files. **No repo id, no base commit, no branch, no host.** `ProposalSet.base_commit` exists (`proposal_sets.rs:290`) and `proposal_preflight::check_base` uses it. |
| Apply | `implement_proposal` (`app/src/lib.rs:6015`), `implement_proposal_targeted` (`:6139`); box `apply_on_this_host` (`serve/api.rs:1365`, opt-in `LIGUS_SERVE_APPLY_ON_APPROVE=1`) | `git apply` into the chat worktree, commit "Apply proposal: <title>", then `ProposalApplied { proposal_id, diff_digest, commit? }` (`events.rs:330`). Repo is "the active workspace's binding". |
| Hosts | `WorkspaceHost` (`crates/hive-core/src/workspace_host.rs`), `WorkspaceHostUpserted` (config log, Contributor), heartbeats via throttled `touch_host` (`chat_service.rs:1721`) | id, kind Device/Worker, label, owner, last_seen. No repo fields. |
| Forges | `crates/hive-forge` | GitHub, GitLab, Gitea/Forgejo/Codeberg. `spec::parse_remote` (`spec.rs:114`) turns https, `ssh://`, scp-style into `(host, RepoRef)`; `same_host` (`:36`) folds port, case, `www.`, `ssh.github.com`. No single canonical-string function. Tokens per connection in `SecretStore`; `forges.toml` device-local; `internal` mark for private addresses. |
| Forge tools | `crates/hive-runtime/src/ligus_mcp/forge_tools.rs` | Read-only, `repo` limited to the chat's bound repo or its upstream (`ForgeBinding::target`), binding from configured origin ignoring `insteadOf`. |

---

## 2. Concepts

### 2.1 Workspace repo (shared)

```rust
// crates/hive-core/src/workspace_repo.rs (new)
pub struct WorkspaceRepo {
    pub id: Uuid,                    // UUIDv5(LIGUS_REPO_NS, canonical)  -- see 2.3
    pub canonical: String,           // "github.com/zazulam/wefit" or "local:<root sha>"
    pub display_name: String,        // "WeFit"; ≤ 80 chars, no control chars
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub forge_kind: String,          // "github" | "gitlab" | "gitea" | "" (unknown)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_branch: String,      // hint; the forge (or the checkout) is authoritative
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_commit: Option<String>, // 40/64-hex; fingerprint of the history (2.4)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,        // other canonical forms of the SAME repo (≤ 8)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_of: Option<Uuid>,   // reserved; see 2.5 (forks are NOT separate repos by default)
    pub added_by: String,            // actor id, informational; the signer is authoritative
}
```

What it is: the workspace's statement "we work on this code". What it isn't:
a path, a URL with credentials, a clone instruction, or a trust grant. Adding
it does not make any device fetch anything.

### 2.2 Checkout mapping (device-local)

```sql
-- <data_dir>/hive.db, never synced, never exported
CREATE TABLE repo_checkouts (
  workspace_id TEXT NOT NULL,
  repo_id      TEXT NOT NULL,
  path         TEXT NOT NULL,          -- canonicalized absolute path
  source       TEXT NOT NULL,          -- 'bound' | 'detected' | 'cloned' | 'migrated'
  primary_     INTEGER NOT NULL,       -- one primary per (workspace, repo)
  confirmed_at INTEGER,                -- NULL = detected but not yet confirmed by a human
  PRIMARY KEY (workspace_id, repo_id, path)
);
```

- The desktop and the box use the same table and the same code (in
  `hive-runtime`, so `ligus serve`, `ligus worker` and the app share it; this
  follows RC-1, one host abstraction).
- `repo-bindings.toml` and `workspace_dirs` become views over it (section 8).
- Per-chat overrides stay possible: `chat_checkouts(chat_id, repo_id, path)`,
  for "this chat uses my second clone". Rare; it replaces `RepoBindings.chats`.

### 2.3 Canonical identity

One function, in `hive-forge` (it already owns remote parsing), used everywhere:

```rust
pub fn canonical_repo(remote: &str, aliases: &HostAliases) -> Result<Canonical, CanonError>;
pub struct Canonical { pub host: String, pub path: String } // to_string() = "host/path"
```

Rules, in order:

| Step | Rule | Example |
|---|---|---|
| 1 | Parse with `spec::parse_remote` (https, `ssh://`, scp-style). Reject `file://`, bare paths, `ext::`, and anything else (those go to step 7). | `git@github.com:ZaZulam/WeFit.git` → (`github.com`, `ZaZulam/WeFit`) |
| 2 | Drop userinfo, port, query, fragment. The port is a transport detail: `ssh://git@host:2222/o/n` and `https://host/o/n` are the same repo. | |
| 3 | Host: lowercase, IDNA to ASCII, strip trailing dot, strip `www.`, map `ssh.github.com` → `github.com` (exactly `spec::same_host`). Then apply workspace host aliases (2.6). | `ssh.gitlab.example.com` → `gitlab.example.com` if aliased |
| 4 | Path: strip leading/trailing `/`, strip one trailing `.git`, collapse nothing else. Reject `.`/`..` segments, empty segments, `%2F`-style encoded separators, control characters, more than 20 segments (matches `parse_repo_spec`). | `/zazulam/wefit.git/` → `zazulam/wefit` |
| 5 | Case: lowercase the path for GitHub, GitLab, Gitea/Forgejo/Codeberg (all case-insensitive for owner/name lookups). For an unknown forge kind, keep case. | `ZaZulam/WeFit` → `zazulam/wefit` |
| 6 | `id = UUIDv5(LIGUS_REPO_NS, "host/path")`. Deterministic, so two admins adding the same repo concurrently get the same id and the fold merges them (first add wins for display fields). | |
| 7 | No usable remote (local-only repo, or only `file://` remotes): `canonical = "local:<root commit>"`, where the root commit is the oldest parentless commit reachable from the default branch (ties: lexicographically smallest sha). Such a repo can only be matched by fingerprint and can't be cloned from a forge. | `local:3f2a…` |

Remotes are read from `git config --get-regexp '^remote\..*\.url$'` without
following `url.<x>.insteadOf` or `pushInsteadOf` (the rule already used by
`configured_remote_url` and `binding_ignores_rewritten_remote`). Rewrites are
a local transport choice and must not change identity, or a hostile global
git config could make an unrelated checkout "match".

`LIGUS_REPO_NS` is a fixed UUID constant, not a signature domain string.

### 2.4 Fingerprint

`root_commit` lets a device check that a checkout whose remote matches is
actually the same history, and lets it suggest a match for a checkout that
has no remote or a renamed one. It is optional (a shallow clone may not have
it; `git rev-list --max-parents=0` on a shallow clone returns the graft
point, which we treat as "unknown"). A mismatch never blocks; it shows
"remote matches but history differs" and requires confirmation.

### 2.5 Forks and personal clones

A fork is **not** a separate workspace repo by default. A checkout covers
repo `R` when, after canonicalization:

1. its `origin` fetch URL equals `R.canonical` or one of `R.aliases`, or
2. any remote named `upstream` equals `R.canonical` (the box's fork layout,
   `setup_fork` at `repos_api.rs:1103`, already names it so), or
3. (only after a human confirms) the root commit matches.

Case 2 is marked `via_fork: <origin canonical>` in the local mapping. That
lets Alice work from `github.com/alice/wefit` and Bob from
`github.com/bob/wefit` and both cover `github.com/zazulam/wefit`, while
pushes keep going to each person's own fork (`push_remote`, `check_push_target`
unchanged). `upstream_of` on `WorkspaceRepo` is reserved for the case where a
workspace really wants the fork itself as a first-class repo (for example a
long-lived downstream fork); it is not needed for G3.

Branches are per chat (2.7), not per repo entry. `default_branch` is only a
hint used when no forge is reachable.

### 2.6 Self-hosted forges and host aliases

- A self-hosted forge's web host and SSH host often differ
  (`git.example.com` vs `ssh.git.example.com`). The workspace may declare
  aliases: `WorkspaceRepo.aliases` per repo, and a small workspace-level map
  `host_aliases: {ssh_host → canonical_host}` (≤ 16 entries) on the same
  config record family. Aliases change identity matching only, never where
  git connects.
- `forge_kind` is a hint for display and the button label ("Open draft MR").
  The device's own `forges.toml` connection for that host is authoritative.
  A host without a connection still matches checkouts; it just can't be
  cloned or listed through a forge.

### 2.7 Chat repo

```rust
// chat log, Session scope
ChatRepoSet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repo_id: Option<Uuid>,          // None = this chat has no repo
    #[serde(default, skip_serializing_if = "String::is_empty")]
    base_branch: String,            // "" = the repo's default branch
}
```

A chat has zero or one repo in this design. Multi-repo chats (one turn
editing app and API together) are an open question (10, Q4); a workspace
having several repos (G4) is covered either way, because each chat picks
one. If a workspace has exactly one repo, new chats get it by default.

The chat branch name stays `hive/chat/<id>` per checkout. It is
device-local scratch state (see 5.5); the synced truth is the ordered list
of applied proposals.

### 2.8 Coverage

```rust
// config log, signed by the host device itself
HostRepoCoverage {
    host_id: String,                       // must equal the signer's device id
    entries: Vec<CoverageEntry>,           // ≤ 64; replaces the previous record for this host
}
pub struct CoverageEntry {
    pub repo_id: Uuid,
    pub state: CoverageState,              // Present | Cloning | Missing | Error(code)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_head: Option<String>,      // commit at the checkout's default branch; lets others judge staleness
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub via_fork: bool,
}
```

Coverage is coordination state (dispatch reads it), not telemetry, so it
belongs on the log under the C.6 split. It is emitted only on change (repo
added or removed, checkout found or lost, clone finished) and at most once
per hour for `default_head` drift, so the volume is similar to host
heartbeats. **No paths, no remotes, no branch names** beyond what the repo
entry already says.

**As built (0.1.0).** Coverage rides the presence beat (`HostBeat.seq`,
`coverage`), not the log. Since the dispatcher review (2026-11-18) each host
signs its report with its own device key (`HostBeat.coverageSig`, Ed25519
over `"ligus-coverage-v1\0" ‖ workspace(16) ‖ device(16) ‖ u64le(seq) ‖
u32le(n) ‖ n × (repo(16) ‖ u32le(len) state ‖ u32le(len) code ‖ via_fork
byte)`; golden vectors in `crates/hive-core/tests/coverage_sig.rs`). A
reader records a beat's coverage only when the signature verifies against
the device its `hostId` names, as its current roster has it (certified, not
revoked, not a phone, acting for a current member); otherwise only the
beat's liveness counts. Unsigned liveness is a residual (11.6).

---

## 3. Where data lives

### 3.1 Synced (signed) vs device-local

| Data | Log | Event | Floor | Extra rule |
|---|---|---|---|---|
| Add a repo | config | `WorkspaceRepoAdded { repo }` | Admin | rules v2: presence (Track B.4). Bounded: ≤ 64 live repos per workspace. Id must equal `UUIDv5(NS, canonical)` or the event folds as a no-op. |
| Edit display name, default branch, aliases | config | `WorkspaceRepoUpdated { repo }` | Admin | No presence: cosmetic. Canonical and id are immutable; changing them is remove + add. |
| Remove a repo | config | `WorkspaceRepoRemoved { repo_id }` | Admin | rules v2: presence. Chats keep their `repo_id`; UI shows "removed repository". |
| Host aliases | config | `WorkspaceHostAliasesSet { map }` | Admin | ≤ 16 entries, each a valid host. |
| Coverage | config | `HostRepoCoverage { host_id, entries }` | Contributor | **Signer must be the device `host_id`**, and that device must be certified and not revoked at the event's causal position. Nobody else can speak for a host. Agent tier: denied. |
| Chat repo | chat | `ChatRepoSet { repo_id, base_branch }` | Contributor | `repo_id` must name a live repo at the author's `cfg_frontier`, else hold/no-op. Agent tier: denied (agents can't redirect a chat to other code). |
| Proposal repo and base | chat | new fields on `ProposalUpserted` (5.4) | unchanged | Covered by the proposal digest (5.4). |
| Applied commit | chat | `ProposalApplied` gains `repo_id`, `applied_on` | unchanged (human Contributor+) | |
| Checkout paths, roots to scan, per-chat local overrides, clone jobs | device | `repo_checkouts`, `chat_checkouts`, `scan_roots` in `hive.db` | n/a | Never in events, invites, contact records, logs at info level, crash reports, the relay or the phone API. |
| Forge tokens | device | `SecretStore` scope `forge` | n/a | Unchanged (identity-and-forges 5.4). |

All new config events go through `authz::authorize` (`crates/hive-core/src/authz/mod.rs:221`)
with `min_role_for` entries (`crates/hive-core/src/authorization.rs:193`). Old
clients fold them to `SessionEvent::Unknown` (inert) and re-sync the raw
bytes, as with every 2.0 addition. All new fields are `serde(default,
skip_serializing_if…)`.

### 3.2 Why Admin for add/remove

Adding a repo looks harmless (it moves no bytes), but it steers what other
hosts offer to clone, what the phone suggests, and what chats default to. It
is in the same class as S2 (agent host binding is Admin,
phase0-specs A.2.5). Contributors can still *use* any listed repo in their
chats. Whether add needs presence is owner question Q2.

### 3.3 Privacy

- Local paths never leave the device. The phone API already follows this
  (`binding_name` shows only the last path component, `external: true`); the
  new endpoints keep it.
- Coverage reveals "host H has repo R checked out" to workspace members. That
  is the point, but a member may not want their laptop listed. Each device
  has "Report coverage for this device". The default is owner question Q6
  (proposed: on for workers and the owner's own devices, opt-in elsewhere). With
  it off the device publishes nothing and others see "unknown", not "missing".
- `default_head` reveals the commit a device is on. It is already implied by
  proposals' base commits. It can be turned off independently.

---

## 4. Detection, binding and cloning

### 4.1 Three ways a mapping gets created

| Way | Who | Where | Confirmation |
|---|---|---|---|
| **Explicit bind** | Human on that device | Desktop: "Choose folder…" (today's `set_workspace_root`), per repo. Box: `ligus repos bind <repo> <path>` (generalizes `ligus bind-repo`). | Implicit (the human picked it). |
| **Detect** | Background scan on that device | Configured scan roots (4.2) plus the box's `repos_dir`. | A detected match is `confirmed_at = NULL` until a human confirms once, on the device (desktop banner) or from the phone for the box ("WeFit found on this host at a checkout named WeFit. Use it?"). Unconfirmed matches show as coverage `Missing` with a hint, never `Present`. |
| **Clone here** | Human, from desktop or phone | Box: generalized `POST /v1/repos/clone {repoId}`. Desktop: "Clone into…" with a folder picker. | Implicit. |

Why detection needs confirmation: once a checkout is `Present`, turns run in
it, and anything a hostile repo planted (hooks, repo config) becomes
relevant. Repo-supplied runtimes and MCP servers are already inert until
trusted (`workspace_trust.rs`), hooks are disabled by `harden_git`
(`crates/hive-core/src/process_util.rs:180`), but a human should still say
"yes, that folder".

### 4.2 Scanning

- Scan roots are device-local (`scan_roots` table). Defaults: the box's
  `repos_dir`; the parent folders of existing bindings on the desktop (so
  binding `~/apps/WeFit` makes `~/apps` a root, after asking). No default
  that covers `$HOME`.
- Never scan shallow or system dirs: reuse `is_shallow_or_system_dir`
  (the v1.10.2 launch-hang guard; `is_auto_adoptable_workspace`,
  `app/src/lib.rs:1330`).
- Depth ≤ 3 below a root, ≤ 2,000 directories visited per root, no symlink
  following, skip `node_modules`, `target`, `.ligus/worktrees`, `.git`
  internals, and any directory that is itself a linked worktree (`.git` is a
  file).
- For each `.git` directory found: read remote URLs from the config file
  (no `git` subprocess needed for the common case; when `git` runs, it runs
  through `harden_git` and `quiet()`), canonicalize (2.3), compare with live
  workspace repos. Read the root commit only for candidates.
- Triggered on: workspace repo added, scan root added, app/box start, and
  "Rescan" in the UI. Not on a timer.
- The box scans only roots configured on the box (`ligus repos roots add
  <path>`, or the desktop on the same machine cannot do it for the box: it is
  a different device). The phone cannot add roots, because that would mean
  sending paths. (Q3: should the phone be able to pick from roots the box
  already has?)

### 4.3 Clone on this host

- `POST /v1/repos/clone` gains `repoId`. The box looks up the repo entry,
  builds the URL with `Forge::clone_url(.., Https)` from the canonical host
  and path, and **requires a forge connection for that host on the box**
  (today's `resolve_spec` rule). `local:` repos can't be cloned.
- The URL is never taken from the event as a string. Only `host` and `path`
  are, after re-validation with `spec::parse_repo_spec` and `valid_host`, and
  only for a host this box already has a connection to (so SSRF goes through
  the connection's `validate_base_url` / `check_address` / `internal` rules,
  `crates/hive-forge/src/conn.rs:108, 171`).
- Credentials stay on the host: `git -c credential.<base>.helper=!ligus forge
  git-credential <id>` (`tokens.rs:398`), never argv or env. The phone never
  sees a token. Existing limits (timeout, size cap, two concurrent jobs,
  hidden temp dir then rename) stay.
- On success the box writes the mapping (`source = 'cloned'`, confirmed) and
  publishes coverage.
- Desktop clone: the same code from `hive-runtime`, into a folder the user
  picks, with the desktop's connection.

### 4.4 Worktree layout

Unchanged for one host per checkout: `<checkout>/.ligus/worktrees/chat-<slug>`
on `hive/chat/<slug>`, turn trees `hive/turn-<slug>`, pool, stopped-turn
patches (`git_worktree.rs`). Multi-repo needs nothing new: each checkout is
its own repository with its own `.ligus/worktrees`.

`chat_base` (`git_worktree.rs:181`) currently reads `hive.chatBase` or uses
`HEAD`. It becomes: the chat's `base_branch` (from `ChatRepoSet`), resolved
as `upstream/<branch>` for a fork checkout and `origin/<branch>` otherwise,
falling back to the local branch, then `hive.chatBase`, then `HEAD`. Using
`HEAD` means "whatever the human has checked out right now", which differs by
device; that is the root of many "stale base" surprises in G1.

### 4.5 Two hosts, one checkout

The owner's case invites pointing the box at the desktop's folder. That is
unsafe as is: both processes would manage `.ligus/worktrees/chat-<slug>` and
the same `hive/chat/<id>` branch, and the hygiene sweep of one would remove
trees the other is using (its `in_use` set is per process,
`git_worktree.rs:30-59`).

Rule: a checkout has one **managing host**. The first host to manage it
writes `.ligus/host` (device id + label, mode 0600). Another host that maps
the same path sees a foreign owner and offers:

1. **Separate clone (recommended):** `git clone --reference <that path>
   <forge url>` into its own `repos_dir`. Fast, shares objects, no shared
   worktrees. Falls back to a normal clone if `--reference` fails.
2. **Read-only use:** turns that never write (scouts, review) may run in a
   detached worktree under `.ligus/worktrees/<host-short>/…`, never touching
   the other host's branches. Writes are refused with "WeFit here is managed
   by Mac app; clone separately to work on it from this box."

Whether to allow option 2 at all is Q5.

---

## 5. Turns and proposals

### 5.1 Chat → repo → checkout → working dir

```
chat.repo_id ──(config log)──▶ WorkspaceRepo
      │
      ▼  on the host that runs the turn
repo_checkouts[(ws, repo_id)].primary  (or chat_checkouts override)
      │
      ▼
chat_worktree(checkout, chat_id) at chat base ──▶ turn worktree ──▶ diff ──▶ proposal
```

On the desktop this replaces "the active workspace's single binding"
(`workspace_repo_root`, `app/src/lib.rs:6717`) with "the chat's repo's
checkout on this device". `apply_chat_dir` (`:6730`) and
`run_prepared_turn_inner` (`:8395`) take the checkout from the chat, not from
the active workspace. A chat with no repo keeps today's behaviour (runs in the
workspace folder if one is bound, or without file tools).

On the box, `repos::resolve` (`repos.rs:80`) gets the same lookup and
`turn_context` (`:470`) names the workspace repo ("Repository: WeFit
(github.com/zazulam/wefit), branch hive/chat/…").

### 5.2 Which host runs a turn: the coverage check

Today: `turn_runs_here` decides by binding (`ThisDevice` / `Elsewhere` /
`Unbound`, `app/src/lib.rs:6921-6941`); `flow::candidates` drops agents on
offline hosts; `can_run_here` (`app/src/flow.rs:1300`) checks the runtime
provider; `ensure_stages_runnable` (`app/src/workflows.rs:1173`) checks
stages. None checks for a checkout.

New: one function, used at all of those sites and at both dispatch gates
(`send_message` and `maybe_respond`, per the two-gates rule):

```rust
pub enum Coverage { Present, Cloning, Missing, Unknown /* host reports nothing */, NotNeeded /* chat has no repo */ }
pub fn coverage(timeline: &RosterState, host_id: &str, repo_id: Option<Uuid>) -> Coverage;
```

| Agent's host | Coverage | Behaviour |
|---|---|---|
| Pinned to H | Present | Dispatch as today. |
| Pinned to H | Cloning | Queue the turn (like an offline host) and say so: "Waiting for WeFit to finish cloning on mac-test." |
| Pinned to H | Missing | **Don't dispatch.** Post a local, non-synced notice to the sender with actions: *Clone on mac-test* (if the sender is Contributor+ and the host is a box they can reach), *Run on <another host with coverage>* (one-turn handoff, `handoff_turn`, `app/src/lib.rs:9646`; doesn't change the pin), *Run without the repo* (explicit, one turn). |
| Pinned to H | Unknown | Dispatch, with a warning chip "mac-test doesn't report its repos". The host itself still refuses at turn time if it has no checkout (5.3). |
| Unbound (owner's device) | the owner has several devices | `turn_runs_here` unchanged for exactly-once; but among the owner's devices, prefer one with coverage (`stage_owner_runs_here` gets a coverage tiebreak). |
| Router (`flow::candidates`) | any | Agents whose host is Missing for the chat's repo are dropped from candidates, like offline ones. |
| Workflow stage | Missing | `ensure_stages_runnable` fails up front with the stage and host named. `offer_stage_here` (`app/src/flow.rs:1310`) may claim a stage only if this device has coverage. |

Coverage is read at the sender's causal position, so two devices agree on
the decision as long as they've folded the same coverage record. A stale
read can only cause a turn to reach a host that then refuses (5.3), never
a turn running in the wrong code.

### 5.3 Host-side refusal

The host that runs a turn re-checks locally: chat has `repo_id`, and this
device has a confirmed mapping whose checkout exists and whose remote still
canonicalizes to the repo. If not, the turn ends with a structured reply
"No checkout of WeFit on mac-test" and the same actions, instead of today's
"runs without file tools / in a scratch dir" (CLI) or "runs in place" (desktop
non-git fallback). Running a repo chat in the wrong directory is the worst
outcome; refusing is cheap.

### 5.4 What a proposal records

`ActionProposal` (`proposals.rs:70`) gains, all optional with `serde(default,
skip_serializing_if)`:

| Field | Meaning |
|---|---|
| `repo_id: Option<Uuid>` | The workspace repo the diff is against. |
| `base_commit: Option<String>` | The turn worktree's base: chat-branch tip on the producing host when the turn was cut. |
| `base_ref: String` | The chat's `base_branch` at the time (e.g. `main`), for display and rebase. |
| `produced_on: String` | Host id that produced it. |

The digest (`diff_digest`, phase0 A.2.2 rule 5, `ligus-proposal-digest-v1`)
does not cover these. If it doesn't, an approval for "this diff to WeFit"
could be applied as "this diff to Infra" by an applier who picks the other
repo. Proposal: `ligus-proposal-digest-v2` = v1's fields plus
`u32le(len) || repo_id || u32le(len) || base_commit`, used whenever
`repo_id` is set; v1 stays for proposals without it. This is a new domain
string, which phase0 0.1 requires to be listed; owner decision Q7.

`ProposalApplied` (`events.rs:330`) gains `repo_id` and `applied_on`. Its
authz adds: `repo_id` must equal the proposal's `repo_id`.

### 5.5 Applying on any device

`implement_proposal` / `implement_proposal_targeted` and the box's
`apply_on_this_host` take the repo from the proposal, not from the active
workspace:

1. Look up this device's checkout for `proposal.repo_id`. None → the Apply
   button shows "Apply on…" listing hosts with coverage (applies there via the
   existing remote-apply path for boxes; for another person's desktop, it
   doesn't offer).
2. `chat_worktree(checkout, chat_id)`.
3. If `base_commit` is present in this checkout and is an ancestor of the
   chat-branch tip: `git apply --3way` onto the tip. Clean → commit (today's
   `commit_worktree`) → `ProposalApplied { commit, repo_id, applied_on }`.
4. If `base_commit` is missing here: `git fetch` it from origin/upstream if
   the forge has it (it won't when the base contains earlier unpushed applies
   from another device); else fall back to today's `git apply` against the
   tip and label the result "applied on a different base".
5. Conflict or failure: nothing is applied (today's no-partial-apply rule).
   Offer **Ask the agent to rebase** (a refine turn on the current base;
   `ProposalRefined`), **Mark stale** (`ProposalMarkedStale`), or **Open in
   editor** (desktop: leaves the conflicted tree in a scratch worktree).

### 5.6 Chat branches across devices

Each device's `hive/chat/<id>` is local. After Alice applies proposal P1 on
her laptop, Bob's box doesn't have P1's commit, so P2 (produced on the box)
has a base without P1, and P1's changes are invisible to the box's agent.

Design: the chat's **ledger** is the ordered list of `ProposalApplied`
events for its repo. A host that is about to cut a turn for a chat replays
any ledger entries its chat branch lacks (it has each diff in the proposal),
in order, on top of the chat base. A replay conflict stops the turn with
"This chat's branch has diverged on mac-test" and the 5.5 actions. This is
the deferred **Phase 2b "pull in"**, made concrete and keyed by repo id.
Pushing chat branches to the forge (as `hive/chat/<id>`) is an optional
accelerator, not the source of truth (Q8).

Promote-to-main, explicit per-chat folder override and auto-apply (deferred
Phase 4) are unchanged by this design; the per-chat override becomes
`chat_checkouts`, and auto-apply applies on the producing host (which by
construction has coverage).

### 5.7 Proposal sets and hygiene

- `ProposalSet.base_commit` (`proposal_sets.rs:290`) stays and gains
  `repo_id`. `proposal_preflight::check_base` already handles "base recorded
  on another device"; it now also checks repo id.
- `run_hygiene_sweep` (`app/src/guards.rs:572`) runs per mapped checkout, not
  only the active workspace's binding, and marks stale only proposals whose
  `repo_id` matches that checkout.
- Workspace trust keys by `root_id = SHA-256(path ‖ first commit)`
  (`workspace_trust.rs:150`). Trust decisions stay per checkout path (that is
  correct: a second clone is a different set of files), but the UI groups
  them under the workspace repo. The existing quirk that a chat worktree path
  gets a different `root_id` from its main checkout should be fixed by
  computing `root_id` from the main checkout path (`git rev-parse
  --git-common-dir`) when inside `.ligus/worktrees`.

---

## 6. UX

### 6.1 Desktop

**Workspace settings → Repositories** (new; replaces "Folder & Git" for
workspaces with repos):

| Repository | This Mac | mac-test (box) | Alice's laptop |
|---|---|---|---|
| WeFit · github.com/zazulam/wefit · main | ✓ ~/apps/WeFit | not cloned · **Clone there** | ✓ via fork alice/wefit |
| Infra · gitlab.example.com/ops/infra | **Bind folder… / Clone…** | ✓ | unknown (doesn't report) |

- Only this device's own path is shown, never others'.
- "Clone there" appears for boxes the current person can drive (their own
  boxes or workers where they're Admin); it calls that box's clone through
  the workspace (a workspace-addressed request to the box, Q9) or, MVP, is
  only offered from the phone.
- Add repository: from this device's checkouts (auto-suggested from remotes),
  from a forge connection, or by URL. Admin only; others see "Ask an admin".

**Chat header:** a repo chip "WeFit · main" (or "No repository"). Clicking
it changes `ChatRepoSet` (Contributor+). If the chat's agent is pinned to a
host without coverage, the chip shows a warning dot and the composer shows
"Pinned agent ScoutBee runs on mac-test, which has no WeFit. Clone there ·
Run here this time".

**Onboarding:** `BindWorkspacePrompt` becomes "Link a folder for WeFit" when
the workspace has repos, and "Link a folder" otherwise. After binding a
folder whose remote isn't a workspace repo yet, an Admin sees: "This folder
is github.com/zazulam/wefit. Add WeFit to the workspace so your other
devices and your phone can find it? [Add] [Not now]". This single prompt is
the fix for the owner's case.

### 6.2 Phone

The phone talks to one box but reads the synced workspace log through it, so
it can show other hosts' coverage without reaching them.

**Repo picker** (`RepoPickerView`), new top section "Workspace repositories":

```
WeFit                      github.com/zazulam/wefit
  On Mac app · Not on mac-test           [Clone here]
Infra                      gitlab.example.com/ops/infra
  On mac-test ✓
```

- "Clone here" = `POST /v1/repos/clone {repoId}` on the connected box.
- A detected-but-unconfirmed match shows "Found on mac-test as WeFit · Use
  it", which confirms the mapping (owner's own phone, through the tunnel; the
  presence rules for box writes apply).
- The existing "This host" and per-forge sections stay below for ad hoc
  clones; cloning something that isn't a workspace repo offers "Add to
  workspace" (Admin).
- Chat chip: "WeFit · main", and when the box has no checkout: "WeFit isn't
  on mac-test. Clone here".
- Empty state when the workspace has no repos: "This workspace has no
  repositories yet. Add one on your Mac, or pick a checkout on this host."

### 6.3 Warnings, in one place

| Condition | Where | Text |
|---|---|---|
| Pinned host lacks the chat's repo | composer, phone chat | "<agent> runs on <host>, which has no <repo>." + actions |
| Chat repo removed from workspace | chat chip | "<repo> was removed from this workspace." |
| Detected match unconfirmed | settings, phone picker | "Found <repo> on <host>. Use it?" |
| Two hosts on one checkout | settings | "<repo> here is managed by <host>." |
| Proposal base not here | Apply button | "Applied on a different base" (after) / "Ask the agent to rebase" (on conflict) |
| Remote no longer matches | settings | "<path name> now points to <other canonical>; it no longer counts as <repo>." |

---

## 7. Security and threat model

Adversaries: a malicious workspace Admin or a compromised Admin device; a
Contributor; an agent (prompt-injected); a hostile repository; a hostile
relay; a malicious self-hosted forge.

| Threat | Mitigation | Test |
|---|---|---|
| Path traversal in a repo entry (`../../etc`, absolute paths, `%2e%2e`) used to build a clone dir | Repo entries carry no paths. Clone dir names come from validated segments (`checkout_for`, `valid_checkout_dir_name`). Canonical path rejects `.`/`..`, encoded separators, control chars. | `repo_entry_traversal_rejected` |
| Hostile remote: `ext::sh -c …`, `file:///`, `fd::`, `--upload-pack=…` as a "URL" | Canonical form is `host/path` only; clone URL rebuilt by `Forge::clone_url(Https)`; `--` before the URL; `protocol.ext.allow=never` (`harden_git`); add `protocol.file.allow=never` for clones by repo id. | `clone_url_rebuilt_never_from_event_string`, `ext_and_file_protocols_refused` |
| SSRF via a self-hosted forge host in a repo entry (`169.254.169.254/x/y`, `localhost`) | A repo entry never causes a request by itself. Clones and forge calls need a device-local connection for that host, which went through `validate_base_url` + `check_address` (public addresses only unless the device marked it `internal`). An Admin can't mark another device's connection internal (the mark is in `forges.toml`, never synced). | `repo_entry_cannot_create_connection`, `internal_mark_not_synced` |
| Repo entry auto-clones on every box | Never automatic in 2.x. Clone is a human action per host. | `adding_repo_moves_no_bytes` |
| Agent adds a repo, changes a chat's repo, or rewrites coverage | Agent tier denied for `WorkspaceRepo*`, `ChatRepoSet`, `HostRepoCoverage` (`authz/agent.rs`). Mappings are device-local and have no MCP tool; the sandbox denies the data dir (phase0 C.4/C.5), so `hive.db` and `repo-bindings.toml` can't be written by agents. | `agent_cannot_set_chat_repo`, `agent_cannot_write_mappings` |
| Contributor fakes another host's coverage to steer dispatch | `HostRepoCoverage` valid only when signed by `host_id`'s own certified device. | `coverage_signed_by_other_device_ignored` |
| Fake coverage makes a turn run in the wrong code | Host-side re-check (5.3) of mapping + remote canonical before every turn. | `host_refuses_turn_without_matching_checkout` |
| Approval re-targeted to another repo | Digest v2 covers `repo_id` and `base_commit`; `ProposalApplied.repo_id` must match. | `vote_for_repo_a_does_not_count_for_repo_b` |
| Look-alike repo (`github.com/zazu1am/wefit`) added to steer clones | Admin-only + presence (rules v2); UI shows the full canonical form and forge owner, and flags a new repo whose root commit equals an existing repo's but whose canonical differs ("same history as WeFit, different owner"). | `lookalike_flagged_by_fingerprint` |
| Remote rewritten via `insteadOf` to match a repo | Identity reads `remote.*.url` raw; rewrites ignored. | `binding_ignores_rewritten_remote` (exists), `detection_ignores_insteadof` |
| Hostile repo content once a checkout is matched (hooks, `core.fsmonitor`, repo MCP/runtimes, `.ligus/` files) | `harden_git` (hooksPath, fsmonitor off); repo items inert until trusted per device (`workspace_trust.rs`); pre-flight never from repo git config (C.3). Detection requires human confirmation. Directory-trust: a repo entry from another member grants nothing about trust; trust is per device and per digest. | `detected_checkout_requires_confirmation`, `untrusted_repo_mcp_never_spawns` (exists) |
| Forge tokens leak through repo work | Tokens stay in the host's `SecretStore`; credential helper only answers for the connection's own host and `https`; phone never holds one. | `no_forge_token_leaves_device` (exists), `clone_by_repo_id_uses_host_credentials_only` |
| Paths leak (phone API, coverage, logs) | No path fields in synced types; phone DTOs carry names only; coverage has no path. Static test over serialized event schemas. | `no_path_in_synced_repo_events`, `phone_repo_dtos_have_no_paths` |
| Unbounded entries (DoS) | ≤ 64 repos, ≤ 8 aliases each, ≤ 16 host aliases, ≤ 64 coverage entries, name ≤ 80 chars, canonical ≤ 300 bytes; over-limit events fold as no-ops (bounds.rs). | `repo_bounds_enforced` |
| Two hosts corrupt one checkout | Managing-host marker (4.5). | `second_host_refuses_foreign_managed_checkout` |
| Relay withholds or reorders repo events | Ordinary log properties; authz at causal position; a missing `WorkspaceRepoAdded` causes `ChatRepoSet` to hold, not to resolve to something else. | `chat_repo_held_until_repo_event_arrives` |

---

## 8. Migration

2.0 starts fresh (no 1.x data). Within 2.0 data, existing bindings become
mappings; nothing is published without a human.

| Today | Becomes | Automatic? |
|---|---|---|
| Desktop `workspace_dirs[ws] = path` | If the folder is a git repo with a usable remote: a candidate repo (canonical from `origin`, or `upstream` when origin is a fork) and a local mapping `source='migrated'`. | Mapping: yes, local. Repo entry: **proposed** to an Admin with the prompt in 6.1; nothing synced until they accept. Non-Admins see "Ask an admin to add WeFit". |
| Desktop folder without git, or git without remote | Stays a plain workspace folder (today's behaviour), or `local:<root>` repo if the Admin chooses. | No. |
| Box `repo-bindings.toml` `workspace = "<name>"` | Mapping for the repo whose canonical matches that checkout's remote; same proposal flow (the phone shows "Add WeFit to the workspace?" to an Admin). | Mapping: yes. Repo: proposed. |
| Box `[chats] "<id>" = "<name>"` | `ChatRepoSet { repo_id }` authored by the box only after the repo exists, plus `chat_checkouts` if the chat used a non-primary checkout. | Once per chat, after the repo is added. |
| `hive.chatBase` git config | Read as a fallback in `chat_base`; the repo's `default_branch` takes over for new chats. | Yes. |
| Existing proposals (no `repo_id`) | Apply as today against the device's single binding for the chat's workspace; shown as "repository unknown". | n/a |

The migration is idempotent (keyed on `(ws, path)`), runs in `build_state`'s
deferred thread on the desktop and at `serve` start, and never scans beyond
existing bindings.

---

## 9. Options, recommendation, plan, tests

### 9.1 Options considered

**(A) Shared repo identity + device-local mappings + coverage** (this
document).
- For: fixes the owner's case and G1-G6 with one model; paths never leave
  devices; dispatch can make a correct decision before sending; proposals
  become repo-addressed; forks work via `upstream` matching; fits the
  config-log/authz machinery already built (one event family, `authorize`,
  bounds).
- Against: new events (4 config, 1 chat), a digest v2 domain string, a
  coverage record that reveals which devices hold which repos, and real work
  in dispatch (two gates, router, workflows). Ledger replay (5.6) is the
  largest piece.

**(B) Keep purely local bindings; sync display-only hints.** Each host
publishes "I have something called WeFit with remote X" as an unsigned or
low-trust hint; nothing in dispatch or proposals changes.
- For: smallest change; no admin flow.
- Against: no shared truth, so two devices still can't agree what "the
  repo" is; hints from each host can conflict; dispatch stays blind (G2);
  proposals still unaddressed (G1); a hint channel the UI trusts but authz
  doesn't is exactly the RC-2 pattern the 2.0 review removed. Fixes the
  phone's empty screen only cosmetically: the box still has no checkout.

**(C) Per-chat repo declared ad hoc.** No workspace list; each chat names a
canonical remote in `ChatRepoSet { canonical }`, hosts match it.
- For: no Admin step; multi-repo is natural; Contributors self-serve.
- Against: every chat re-declares; typos and look-alikes per chat; no place
  to hang aliases, display names, default branches or coverage summaries; the
  phone can't show "workspace repos"; authz is weaker (any Contributor
  steers any host's clone suggestions). It is a subset of A: A's
  `ChatRepoSet` could later accept an ad hoc canonical for one-off chats if
  wanted (Q4).

**(D) Considered and rejected:** sync paths per device (privacy, and paths
are meaningless elsewhere); make the box mirror the desktop's checkout via
file sync (files never sync, by design); push every chat branch to the forge
as the source of truth (needs write tokens everywhere and leaks WIP to the
forge).

### 9.2 Recommendation

**A**, phased so the owner's case ships first and dispatch/proposal changes
follow once identity and coverage have run for a while.

### 9.3 What not to change

- Paths stay device-local; the phone API keeps sending names only.
- Forge tokens stay per host; clones stay human-initiated.
- Per-turn proposals and per-chat worktrees stay (v1.11 Option 2).
- `/v1` relay bodies are untouched: everything here rides existing log sync.

### 9.4 Phased plan

| Phase | Scope | Done when |
|---|---|---|
| **1. MVP: the owner's case** | `canonical_repo` in hive-forge; `WorkspaceRepo{Added,Updated,Removed}` + authz + bounds; `repo_checkouts` table shared by desktop and box; desktop "Add WeFit to the workspace?" after binding and in migration; box: scan `repos_dir` + configured roots, `ligus repos roots add`, `ligus repos bind`, `POST /v1/repos/clone {repoId}`; `HostRepoCoverage`; `GET /v1/workspace-repos` (repos + coverage per host, no paths); phone "Workspace repositories" section with coverage and Clone here; managing-host marker (4.5). | Owner binds `~/apps/WeFit` on the desktop, accepts the prompt, opens the phone: "WeFit · On Mac app · Not on mac-test · Clone here"; taps it; the box clones, coverage flips, a chat on the box runs in WeFit. |
| **2. Chats and dispatch** | `ChatRepoSet`; desktop and box resolve the turn dir from the chat's repo; `chat_base` from `base_branch`; `coverage()` at both dispatch gates, `flow::candidates`, `ensure_stages_runnable`, `offer_stage_here`; host-side refusal (5.3); composer warnings and handoff actions; per-checkout hygiene. | A pinned agent on a host without the repo never starts a turn there; the sender sees why and can clone or hand off. |
| **3. Proposals across devices** | `repo_id`/`base_commit`/`base_ref`/`produced_on` on proposals; digest v2; `ProposalApplied.repo_id`; apply-anywhere with `--3way` and the conflict actions; ledger replay (5.6, the deferred 2b). | A proposal made on the box applies on the desktop and vice versa; stale bases are handled without partial applies. |
| **4. Forks, aliases, multi-person polish** | Host aliases, look-alike flagging, "via fork" coverage, per-chat checkout override (deferred Phase 4), coverage opt-out, optional forge push of chat branches. | Two people with their own forks both show coverage of the upstream repo and can apply each other's proposals. |

### 9.5 Executable tests the implementation must ship

Unit and property (hive-forge, hive-core):

- `canonical_ssh_https_scp_equal`: `git@github.com:A/B.git`, `ssh://git@github.com:22/a/b`, `https://www.github.com/a/b/`, `ssh.github.com:443` forms all give `github.com/a/b`.
- `canonical_self_hosted_with_port_and_alias`: `ssh://git@ssh.git.example.com:2222/grp/sub/repo.git` with alias → `git.example.com/grp/sub/repo`.
- `canonical_unknown_forge_keeps_case`, `canonical_rejects_traversal_and_encoded_separators`, `canonical_local_repo_uses_root_commit`, `prop_canonical_idempotent` (canonical of canonical is itself).
- `repo_id_deterministic_concurrent_adds_merge`: two Admins add the same repo on two devices; one repo after fold, either order.
- `repo_events_need_admin`, `repo_add_needs_presence_under_rules_v2`, `repo_bounds_enforced`, `agent_cannot_add_repo`.
- `coverage_signed_by_other_device_ignored`, `coverage_from_revoked_device_ignored`.
- `chat_repo_held_until_repo_event_arrives`, `agent_cannot_set_chat_repo`.
- `vote_for_repo_a_does_not_count_for_repo_b` (digest v2), `legacy_proposal_digest_v1_unchanged`.
- `no_path_in_synced_repo_events` (schema walk over all new event types).

Integration (temp repos, loopback fake forge, simulated hosts; no real forge, `LIGUS_FORGE_DENY_REAL=1`):

- **Owner case:** `desktop_bind_proposes_repo_box_detects_and_phone_sees_coverage`: desktop binds a checkout, Admin accepts, box with no checkout reports Missing, `GET /v1/workspace-repos` shows "on desktop, not on box"; clone by `repoId` from the fake forge; coverage Present; a chat turn on the box runs in it.
- **Multi-device:** `three_devices_different_paths_same_repo_cover`: paths differ, names differ, all Present; `prop_exactly_once_dispatch` extended with coverage (N hosts, random coverage and delivery, each turn runs once and only on a covering host or is refused with a notice).
- **Multi-person:** `two_members_two_forks_cover_upstream` (origin = own fork, upstream = workspace repo); `contributor_can_use_but_not_add_repo`.
- **Forks:** `fork_checkout_bases_chat_on_upstream_default`, `push_still_goes_to_own_fork` (existing `check_push_target` semantics preserved).
- **Missing checkout:** `pinned_host_without_checkout_never_runs_turn`, `host_refuses_turn_without_matching_checkout`, `workflow_stage_on_uncovered_host_fails_preflight`, `offer_stage_here_requires_coverage`, `remote_changed_drops_coverage`.
- **Stale base:** `apply_on_other_device_with_known_base_three_way_clean`, `apply_conflict_applies_nothing_and_offers_rebase`, `apply_with_unknown_base_labels_different_base`, `ledger_replay_brings_chat_branch_up_to_date`, `ledger_replay_conflict_stops_turn`.
- **Hostile entries:** `repo_entry_traversal_rejected`, `clone_url_rebuilt_never_from_event_string`, `ext_and_file_protocols_refused`, `repo_entry_for_private_ip_host_cannot_clone_without_internal_mark`, `detection_ignores_insteadof`, `detected_checkout_requires_confirmation`, `lookalike_flagged_by_fingerprint`, `oversized_repo_event_noop`.
- **Same machine:** `second_host_refuses_foreign_managed_checkout`, `reference_clone_shares_objects_not_worktrees`.
- **Privacy:** `phone_repo_dtos_have_no_paths`, `coverage_opt_out_shows_unknown_not_missing`, `no_forge_token_leaves_device` (existing, extended to clone-by-id).
- **Migration:** `migrated_binding_creates_local_mapping_only`, `migration_idempotent`, `box_chat_bindings_become_chat_repo_after_repo_added`.

Phone (ligus-ios, against the mock host): picker shows workspace repos with
coverage, Clone here starts a job, unconfirmed match confirm flow, chip
warning when the box lacks the chat's repo.

---

## 10. Open questions for the owner

1. **Q1 Scope of MVP.** Is Phase 1 (identity + box detect/clone + phone coverage) enough to ship in a 2.0.x, with chat repos and dispatch checks (Phase 2) in 2.1? Or must dispatch refusal land together so a pinned agent can't run repo-less?
2. **Q2 Presence for repo add/remove.** Admin only, or Admin + passkey presence under rules v2 (like agent host binding)? Presence is safer against a compromised Admin device steering clones; it adds a tap for a common action.
3. **Q3 Box scan roots.** Should the box scan only `repos_dir` plus roots added on the box's CLI, or may the phone pick from a short list of roots the box already has (names only)? On your Mac, do you want the box to see `~/apps` at all, or always keep its own clones?
4. **Q4 Multi-repo chats.** One repo per chat (this doc), or allow a chat to span repos (one turn editing app and API)? The latter needs proposals per repo and grouped approval.
5. **Q5 Same-machine sharing.** When the box and the desktop are on one Mac, allow read-only box turns in the desktop's checkout (4.5 option 2), or always require a separate `--reference` clone?
6. **Q6 Coverage reporting default.** On for all devices, or on only for workers and the owner's own devices, off for other members' personal machines unless they opt in?
7. **Q7 Digest v2.** Accept a new domain string `ligus-proposal-digest-v2` so approvals bind to a repo and base, or bind via `ProposalApplied` checks only (weaker: the vote itself isn't repo-specific)?
8. **Q8 Chat branches.** Ledger replay as the cross-device source of truth (this doc), or push `hive/chat/<id>` to the forge when the host has write access? Also: rename the `hive/` branch prefix to `ligus/` in 2.x, given the rebrand?
9. **Q9 Driving another host.** Should the desktop be able to ask a box to clone (a workspace-addressed request carried over the log or the tunnel), or is that phone-only for now?
10. **Q10 Unknown forges.** For a host with no connection and unknown kind, keep case-sensitive paths (safe, may split one repo into two entries) or lowercase everything (simple, may merge two repos on a case-sensitive server)?

---

## Implementation notes (0.1.0)

Built: canonical identity (`hive_core::workspace_repo::canonical_repo`), the
repo fold and its order rules, the chat ledger (`ChatSession::applied_order`,
`workspace_repo::ledger`), device-local tables (`repo_checkouts`,
`chat_checkouts`, scan roots, SSH host aliases), `hive_runtime::repos` (bind,
detect, the managing-host marker, coverage, the turn gate, `prepare_chat` with
pinned bases and ledger replay, scratch-tree compare-and-swap apply, clone by
id), the box's turn gate, clone by id and `/v1/workspace-repos` API, the
desktop's turn gate and repo-addressed Implement, Settings → Repositories, the
chat repo chip, the add-this-repo prompt, `ligus repos …`, per-checkout
hygiene and preflight, router and workflow coverage checks, look-alike flags.

Also built (R9 and the 6.1/6.3 desktop pieces): per-turn resolution on the
desktop (review F14): `turn_scope` resolves a turn's directory, runtimes, MCP
servers and trust from its own chat (a repo chat's chat worktree in this
device's checkout; another chat's worktree in the workspace's folder) before
any tool loop, and re-resolves the responder's runtime against that catalog;
the global root, catalogs and watcher follow only the viewed chat, and
viewing never cuts a chat branch. The desktop's "Clone into…"
(`repos::clone::clone_into`, the box's safe path: URL rebuilt from the
canonical for a connected host, address re-check and pin, helper-only
credentials, temp dir and rename, identity origin, claimed-root check). The
composer warning ("‹agent› runs on ‹host›, which has no ‹repo›. Clone there
· Run here this time"), where "Clone there" points to the phone and "Run here
this time" claims that one turn on this device when it has the repo, runs the
agent's runtime and the agent is this person's. Apply-conflict actions: "Ask
the agent to rebase" (a follow-up turn carrying `apply::conflict_probe`'s
conflict and the branch tip; the revision supersedes and needs a vote) and
"Open in editor" (the chat's folder via the OS opener, no shell). Desktop
command-layer tests for these, the gate, apply and clone run in the in-app
harness's offline checks (`app/src/e2e.rs`, `--features e2e`).

Also built after the adversarial review of those pieces (2026-11-18): review
residual 3, re-pinning. `ChatBasePinned` is first pin wins per repo
(`ChatSession::repo_bases`, kept across repo switches); the explicit
`ChatBaseRepinned {repo_id, commit, previous}` needs an Admin or the chat's
creator, a human on their own device, presence under rules v2, and `previous`
equal to the pin in force (a compare-and-swap in authz and in the fold).
`prepare_chat` follows a re-pin when the chat branch was cut from an earlier
pin (`apply::branch_cut_point`): `apply::rebase_onto_repin` rebuilds the branch
on the new pin from the ledger in scratch indexes and moves it by `reset
--keep` in a clean chat worktree or `update-ref <new> <old>`, or refuses
visibly. Residual 6: hygiene and legacy turns and applies (a chat without a
repo, in the bound folder; the box's legacy binding) check
`marker::managed_elsewhere` and leave another host's checkout alone. Per-turn
scope also covers `implement_proposal`'s command run (its directory and
sandbox policy from the chat's own catalog), the legacy apply's folder, the
tool loop's reply root and the Ligus forge tools' binding. "Clone into…"
never follows a redirect, pinned or not; "Open in editor" opens only a real
folder inside the checkout and refuses packages on macOS; "Run here this
time" is honoured only in the case the composer offers it.

Not built yet: the phone UI (in the separate phone app repository; the box API
it needs is here), "Clone there" from the desktop to another host (the review
keeps it phone-only), and renaming the `hive/` branch prefix.

Built since (2026-11-18, branch `feat/device-dispatcher`, reviewed and
completed on `feat/device-dispatcher-v2`): the device-keyed dispatcher
(section 11: `hive_core::dispatch`, `ChatService::dispatch_turn`, both
desktop gates, the deferred claim, hand-offs, the worker, workflow stage
offers, the desktop's revision and regenerate turns) and workspace-synced
repo and host aliases (section 12); signed coverage (2.8, repos review
residual 1); the box's repo actions limited to its owner (repos review F12
and residual 2: clone by spec or id, rename, set-chat-repo, rescan and
confirm need a phone certified to the account of the Admin who admitted the
box, or the `serve.token` bearer on a loopback listener; 403
`owner_required` otherwise). Still open: the phone app shows the aliases the
box API lists only once the phone repository adopts them.

---

## 11. The device-keyed dispatcher (architecture-review 4.9; review F8, R8)

Status: designed 2026-11-18 for Ligus 0.1.0 (Ligus is short for
*ligustica*). Built in `hive_core::dispatch` (the fold and the decision,
both pure) and `ChatService::dispatch_turn` (the one entry every dispatch
site calls).

**Invariant.** Each turn (a claim key: a user message, or a hand-off's
`(trigger, receiver)` key, `turn_claim_key`) has **at most one committed
answer** in every device's fold, whatever arrives in what order, and, once
partitions heal and some eligible device stays up, **exactly one**.
Exactly-once *execution* isn't achievable with devices that go offline
mid-turn; what is guaranteed is that a second execution never commits.

### 11.1 What is signed

`TurnClaimed` gains three optional fields (additive; absent = today's
claim, so an older claim folds exactly as before):

| Field | Meaning |
|---|---|
| `epoch: u32` | The claim generation for the key. Absent = 1. |
| `supersedes: Option<Uuid>` | The holder this claim takes the turn from (a hand-over). Required for `epoch > 1`. |
| `repo_id: Option<Uuid>` | Signed coverage: "this device has a confirmed checkout of this repo, which is the chat's". The claim is signed by the device it names, so the attestation is the host's own. |

Coverage on the presence beat stays advisory (it is unsigned and differs per
reader), so it never decides anything; it only tells a waiting device
whether to keep waiting.

The answer itself is the commit: an assistant or agent message with
`replyToMessageId` and `originDeviceId`, where the origin is verified
against the envelope (the signer is that device, or an agent key that
device certified). A reply to a System row that trails the trigger
counts for the trigger (the placeholder rule in `run_prepared_turn`). An
answer without a verified origin (an older build) is left alone: it is
neither a commit nor fenced.

### 11.2 The fold (canonical `(lamport, event_id)` order; pure, on every device)

Per key the fold keeps a slot `{holder, epoch, covers, answer}`
(`ChatSession::turn_dispatch`; `turn_claims` stays the holder map every
existing reader uses).

1. A claim for a key that already has an answer is ignored.
2. First claim (`epoch` 1, no `supersedes`): holder. A takeover whose
   predecessor isn't folded yet is ignored until it is (the full re-fold
   applies it then).
3. Same epoch, different device: the first wins, **except** that a covering
   claim (`repo_id` equals the chat's repo at that point) displaces a
   non-covering holder. Once the holder covers, nothing displaces it at that
   epoch.
4. Hand-over: `epoch == current + 1` and `supersedes == current holder`
   (at most 64 epochs, unless the holder is gone at the claim's position).
   A claim naming a stale holder or skipping an epoch is ignored, so two
   devices that take over from the same holder at once converge on the
   first in canonical order, and a third must name the new holder. Since
   the review: never to the holder itself, a device holds one turn at most
   twice (`MAX_HOLDS_PER_DEVICE`), and a hand-over signed (envelope
   timestamp) less than `CLAIM_TIMEOUT - HANDOVER_SKEW` (15 s) after the
   holder's claim is ignored unless the holder is gone at its position. The
   slot keeps the displaced holders and when each was displaced (`before`).
5. Answer: the first verified answer from the holder at its position
   commits. A displaced holder's answer signed before the hand-over that
   displaced it also commits, and displaces an answer a later holder
   committed (so a member can't slip a hand-over in by lamport between a
   holder's claim and the answer it wrote at once, then answer itself).
   Every other answer to that key (from a non-holder, or after a commit) is
   marked `superseded` and is hidden from every view (the transcript on the
   desktop and phone, notifications and push, the answered checks,
   cascades, the next turn's context and summaries, exports, the facts
   export, the MCP read tools, workflow stage outputs). An answer by
   another agent than the committed one (a message that mentions several,
   each run where it lives) is its own turn under `(trigger, responder)`.
   An answer to a key nobody claimed (a local cascade, a relay-less device)
   commits under the trigger and under `(trigger, responder)`, so a late
   claim can't run it again. The origin is the signer, or the host that
   certified the signing agent key, as the workspace roster at the answer's
   position has it.
6. Removing the committed answer (regenerate, on any device) starts the
   turn over: its slot and claim are dropped, so the next answer commits
   whoever writes it.

Because every rule reads only the folded state at the event's position,
the result is a function of the event set: any arrival order converges.
At most one answer commits per key because commits happen only in rule 5,
which admits one.

### 11.3 Authz, revocation and backdating

Unchanged machinery does the work: a claim must be signed by the device it
names (or by an agent key that device certified), by a member at its config
frontier. A revoked device's events that its revocation's watermark doesn't
cover never fold, and a removed member's are judged as of the removal,
whatever frontier or lamport they claim (the lamport is bounded by its
`logHead`, `crate::lamport`). So a backdated claim or answer from a revoked
or removed device never becomes holder and never commits. A claim made
*before* the revocation (covered by the watermark) stays folded; the
decision below treats its holder as gone and hands the turn over at once.

### 11.4 The decision (pure; `dispatch::decide`)

Inputs: the folded slot, this device, whether it is **eligible**
(`turn_runs_here`: local-only and solo devices always are; a pinned agent's
host is; the owner's devices for unbound agents), whether it **covers** the
chat's repo (the host gate's own check: a confirmed mapping whose remote
still is the repo), how long it has waited, the trigger's age, and whether
the holder is live (its presence beat) or gone (revoked, or its account no
longer a member, in the current roster).

| Slot | Decision |
|---|---|
| answered | stand down |
| holder is me | run |
| no holder, not eligible | stand down (the deferred fallback below may still claim) |
| no holder, repo chat, I don't cover, waited < 8 s | wait (covering devices claim first) |
| no holder | claim epoch 1, with `repo_id` when I cover |
| other holder that doesn't cover, I cover | claim the same epoch with `repo_id` (displaces it) |
| other holder gone | claim `epoch + 1`, `supersedes` it, now |
| other holder silent for `CLAIM_TIMEOUT` (25 s) and not live, trigger younger than 15 min | claim `epoch + 1`, `supersedes` it |
| other holder live but no answer for `HOLDER_COMMIT_TIMEOUT` (120 s); a covering holder only by a covering device | claim `epoch + 1`, `supersedes` it |
| I held it twice already, or epoch 64 and the holder isn't gone | stand down |
| otherwise | wait and look again |

A claim is followed by a re-fold; the device runs only if it is then the
holder. A non-covering holder of a repo chat runs the host gate, which
refuses with one reason ("No checkout of WeFit on mac-test"), so a turn
either runs on a covering host or yields a refusal. A host that is
**cloning** covers for claiming purposes only after the clone (it reports
`cloning`), but it claims at once as an eligible host and then waits: it
is live, so no takeover fires (review F8's race).

### 11.5 The two gates and the deferred claim

- `turn_runs_here` keeps deciding **eligibility** at both gates
  (`send_message`, the local send, and `maybe_respond`/`respond_once`, the
  relay-sync path) and at `handoff_turn` and the worker's `drain_tick`; a
  solo or relay-less device is always eligible. Both gates call
  `ChatService::dispatch_turn` instead of `claim_turn`.
- `DEFERRED_CLAIM_TIMEOUT` is `dispatch::CLAIM_TIMEOUT`. The deferred
  fallback (a device that wasn't eligible waits it out) now asks the
  dispatcher as an eligible device: an unclaimed turn is claimed at
  epoch 1 as before, and a turn whose holder went silent is handed over
  instead of being left unanswered.
- A device that is eligible but lost the claim (or sees a foreign claim on a
  synced trigger) arms the same watcher, so a holder that dies after
  claiming is replaced.
- The streaming placeholder is no longer the second-line guard: it is the
  commit, and a duplicate is fenced.

### 11.6 Mixed versions and residual risks

- An older build folds `TurnClaimed` as first-claim-wins and ignores
  `epoch`, `supersedes` and `repoId`; it never hands over and never marks a
  duplicate superseded. A newer device's hand-over or coverage preemption
  can therefore leave an older device showing the original holder; if that
  older device is the original holder and comes back, it may answer, and
  the newer devices hide its answer while the older ones show both.
- The field names follow the existing `turnClaimed` payload (snake case:
  `trigger_id`, `device_id`, `epoch`, `supersedes`, `repo_id`).
- A holder that dies mid-stream has committed; its partial answer stays
  (as today, until its own stale-stream sweep).
- Side effects of a fenced execution (a proposal it filed, a workflow it
  started) are not retracted; proposals still need their approvals.
- Liveness relies on wall-clock timers and the presence beat; with every
  eligible device partitioned from the holder for longer than
  `CLAIM_TIMEOUT`, two may execute and one answer is hidden. "How long" is
  one clock per holder and epoch in `ChatService` (every gate, the watcher
  and the worker share it), so a new holder starts it over.
- Eligibility stays local: the fold accepts any member device's claim. A
  hostile member's device can claim turns it isn't meant to answer and
  answer them (as any member can post a reply); what the review closed is
  that it can't hold one forever (a live holder that doesn't answer is
  replaced after 120 s, a device holds a turn at most twice), can't lock it
  by chaining hand-overs to the epoch cap without as many devices, and
  can't retroactively fence an answer written promptly. A member with many
  certified devices can still exhaust a turn's epochs; removing the member
  makes its holds gone, which a hand-over may pass the cap for.
- Timestamps are the signer's word: a hostile member can sign a hand-over
  in the future, which only helps the displaced holder's answer.
- Presence liveness is unsigned: a member can make a dead holder look live,
  which delays a hand-over by at most `HOLDER_COMMIT_TIMEOUT`.

Tests: `prop_dispatch_converges_and_commits_at_most_once` (N simulated
devices with random partitions, delivery orders, crashes, coverage and
eligibility, a revoked device writing backdated events, and a hostile
member's device that stays live, claims with forged coverage, hands over
to itself and others with backdated lamports and chosen times, and
answers; every local view at every step has at most one committed
answer; all views converge; after healing, exactly one when an eligible
device stays up, from a covering device for a repo chat), plus unit tests
for each fold rule, revoked and removed holders, backdated claims and the
two gates.

## 12. Workspace-synced repo and host aliases (Phase 4, review F6, R2)

Status: designed 2026-11-18 for Ligus 0.1.0.

Device-local SSH host aliases (`ligus repos alias`) stay as they are.
Two synced kinds join them, on the workspace config log:

| Event | Payload | Floor |
|---|---|---|
| `workspaceRepoAliasesSet` | `repo_id`, `aliases: [canonical]` (replaces the list; at most 8) | Admin, presence (rules v2), never an agent |
| `workspaceHostAliasSet` | `alias` (a host), `host` (a host; `""` clears) | Admin, presence (rules v2), never an agent |

**Names only.** An alias is a normal-form canonical (`host/owner/name`,
never `local:`) or a bare host name: no scheme, user, port, path to a
checkout or address. Nothing in an alias is ever used to build a clone or
fetch URL; clones keep using the repo's canonical through the device's own
forge connection.

**Disjointness** (checked at the event's position; the first in canonical
order wins and a conflicting later event is denied, so the fold is
order-independent):

- A repo alias is not a live repo's canonical, not another live repo's
  alias, and not listed twice.
- **Look-alikes.** Every canonical, alias and host is compared by a
  skeleton, the same fold as the repo look-alike flags
  (`confusable_skeleton`: ASCII-lowercased, `0→o`, `1/i/|→l`, `5→s`, `8→b`,
  `_`/`.`/`-` dropped, then `rn→m`, `vv→w`; `local:` names exactly). An alias whose skeleton equals a live canonical's or
  another alias's is refused, except an alias that differs from its own
  repo's canonical only in ASCII case (the case-split fix review Q10
  recommends).
- A host alias maps to the canonical host of a live repo, isn't itself a
  live repo's host (it would shadow it), isn't a look-alike of one, and
  there are at most 16.
- `workspaceRepoAdded` is also refused when its canonical is a live alias,
  a look-alike of one, or sits on an aliased host (an alias already owns
  those names). Removing a repo drops its aliases.

**Local confirmation.** A synced alias changes nothing on a device until a
person there accepts it (`accepted_aliases` in `hive.db`, keyed by alias
and target, so a changed target needs a new acceptance; the desktop sends
the target it showed, and accepting refuses when the workspace has since
pointed the alias elsewhere). An acceptance counts only while the roster a
device reads names that alias with that target: without a roster (a bind
that reads the device's own fold of the config log, never a cached
acceptance alone) only the device's own SSH aliases count. Accepted aliases
join identity matching only (scan, bind, coverage, the turn gate); a match
that needed one shows "via alias". A detected checkout is still recorded
unconfirmed.

Tests: `alias_change_requires_presence_v2`, `agent_cannot_set_aliases`,
`alias_collision_noop_order_independent` (property over orders),
`alias_lookalike_refused`, `host_alias_must_target_live_repo_host`,
`repo_add_shadowed_by_alias_refused`, `synced_alias_unused_until_accepted`,
`changed_alias_target_needs_new_acceptance`,
`clone_url_never_from_alias`, golden vectors for both kinds.

# Hive on iPhone: thin client + home host

> The iPhone app's source now lives in a separate repository. This document
> stays here as the design and **API contract of `ligus serve`**, which any
> client can implement.

A single-user iOS app for one person's Hive workspace. The
phone doesn't run Hive. It's a thin client over Tailscale to **`ligus serve`**,
a home-host process on an always-on box in the lab. The home host is a normal
Hive device in the workspace: it keeps the E2EE keys, syncs with the relay,
and runs agent turns on the lab's model gateway. The phone gets a small JSON
API plus a Server-Sent Events stream.

```
 iPhone (Hive app)                         always-on box (tailnet)                     elsewhere
 ─────────────────                         ──────────────────────────────────────      ─────────
  SwiftUI client  ── HTTP+SSE, bearer ──▶  hive serve                                  relay (E2EE
  token in Keychain    (tailnet only)      ├─ API + SSE   (crates/hive-cli/src/serve)   opaque blobs)
                                           ├─ sync loop   ── push/pull envelopes ──▶      ▲
  ntfy app  ◀── push (Click: hive://…) ──  ├─ agent worker (crates/hive-cli/src/worker)    │
                                           │   └─ tool loop ── OpenAI wire ──▶ llama.cpp   desktop app
                                           │      (built-in file tools + MCP)   gateway    (same workspace)
                                           ├─ ntfy notifier ──▶ self-hosted ntfy
                                           └─ /v1/dashboard ──▶ workspace metrics, Prometheus, JSON URLs, MCP tools (host side)
```

## Components

- **`ligus serve`** (`crates/hive-cli/src/serve/`) runs in one process:
  - the relay sync loop (the same push/fetch/verify as `ligus sync`, every 3 s
    and immediately after a local write, with exponential backoff on errors);
  - the agent worker, on its own thread (`worker.rs`, shared with
    `ligus worker`). It answers `@mentions` for the agents whose host is this
    box: it claims the turn, streams the reply, checkpoints it, and files the
    turn's file changes as a review proposal;
  - the HTTP API and the SSE stream (`api.rs`, axum);
  - an event hub (`hub.rs`). After every sync, write or turn step, a differ
    compares the projected chats with the last snapshot and emits
    `message.created`, `message.done`, `proposal.created`, `proposal.updated`
    and `chat.updated`.
    A reply the desktop streamed and synced in therefore looks the same to the
    phone as one produced here. Deltas and turn status come straight from the
    worker.
- **Agent turns** run the offline harness's loop for local model runtimes:
  `dispatch::stream_with_tools` with `CompositeTools` (Hive's built-in file
  tools plus the MCP tools from `LIGUS_MCP_CONFIG`), under the same policy as
  the desktop. See "Tool policy" below.
- **Runtimes.** An agent runs on its workspace runtime (config log). Failing
  that, it runs on this host's own `runtimes.toml` entry for its runtime id, or
  on the catch-all `id = "*"` entry, which is typically the lab's keyless
  llama.cpp gateway. Desktop agents are bound to the desktop's device-local
  runtime ids, so this mapping is what lets you send them here.
- **Repos.** Agents work on checkouts under `LIGUS_REPOS_DIR`
  (default `<LIGUS_DATA_DIR>/repos`). `ligus bind-repo <name|path> [--chat <id>]`
  binds the workspace (or one chat) to a checkout in
  `<LIGUS_DATA_DIR>/repo-bindings.toml`. This is the device-local equivalent of
  the desktop's workspace folder binding. The phone can do the same: list the
  box's checkouts and the GitHub repos its `gh` login can see, clone one, switch
  the chat it's in onto it, pull, and open a pull request (see "Repos" below).
  Each turn's system prompt names the repo and branch the chat is bound to now.
- **Notifications** go to ntfy: one when an agent reply finishes and one when
  a proposal is filed.

## API contract

JSON, camelCase, ISO-8601 UTC timestamps (`2026-10-20T17:03:11.52Z`). Every
request needs `Authorization: Bearer <token>`, except `GET /v1/health`.
A missing or bad token gets `401 {"error": …}`. Errors are
`{"error": "<message>"}` with 400, 401, 403 (roster role too low), 404 or 409
(apply conflict). The server binds to its configured address: default
`127.0.0.1:8787`, and on the home host its Tailscale IP. It refuses `0.0.0.0`
and public addresses unless `LIGUS_SERVE_ALLOW_ANY_BIND=1`.

| Method | Path | Response |
|---|---|---|
| GET | `/v1/health` | `{ok: true, version, workspace?, deviceLabel?, identity?}`. `workspace` (room name), `deviceLabel` and `identity` appear only when authorized. `identity = {mode: "owner"\|"member"\|"pending"\|"revoked", actorId, displayName, deviceFingerprint}`: who the phone acts as (see "Your own device" below) |
| GET | `/v1/chats` | `{chats: [{id, title, updatedAt, preview, unread: int, channelId, participants: [Participant]}]}`, newest first |
| POST | `/v1/chats` `{title?}` | `{chat}` |
| GET | `/v1/chats/{id}/messages?after=<messageId>&limit=<n>` | `{messages: [Message]}`, oldest first. Without `after`, the latest page (default limit 100, max 1000) |
| POST | `/v1/chats/{id}/messages` `{body}` | `{message}`. `@Name` mentions route to agents exactly as on the desktop/CLI |
| GET | `/v1/chats/{id}/details` | The chat's details (the desktop's right-hand panel, read-only); see below |
| GET | `/v1/agents` | `{agents: [{id, name, runtimeLabel, model?, hostId?, hostLabel?, hostKind, hostOnline?, role?, avatarUrl?, avatarColorHex?, ownerActorId?}]}`. `ownerActorId` (absent when the agent has no recorded owner) is the account that may grant the agent's permission requests besides the host's own person: show the Allow button on a `permission` ask only when it is yours |
| GET | `/v1/chats/{id}/asks` | `{asks: [Ask]}`, newest first. `Ask = {id, chatId, turnId, agentKey, agentName, kind, question, options, expiresAtMs, closed, open, answer?, answeredBy?}`; `kind` is `question` (an agent's `ligus_ask_human`) or `permission` (an ACP permission escalation); decide on `kind`, never on the question's text. `ask.pending` events carry the same `Ask` |
| GET | `/v1/people` | `{people: [Person]}`, the workspace's human members |
| GET | `/v1/channels` | `{channels: [{id, name, purpose?, position, archived, defaultChatId}]}`, in sidebar order |
| GET | `/v1/hosts` | `{hosts: [{id, label, isThis: bool, online: bool}]}` |
| PUT | `/v1/agents/{id}/host` `{hostId}` | `{agent}`. `hostId` must be a registered host. `""` hands the agent back to its owner's device |
| GET | `/v1/proposals?status=open\|all` | `{proposals: [{id, chatId, title, body, kind, diff?, status, createdAt, author}]}` |
| POST | `/v1/proposals/{id}/approve` | `{proposal}` |
| POST | `/v1/proposals/{id}/reject` `{reason?}` | `{proposal}` |
| GET | `/v1/proposals/{id}` | `{proposal}` |
| GET | `/v1/proposals/{id}/file?path=&side=old\|new` | `{path, side, content, size, base, applied}`: one file of the proposal's diff, whole, before (`old`) or after (`new`, the default) the change. Read from the chat's bound checkout (branch `hive/chat/<id>`, else `HEAD`) with the file's patch applied in a scratch directory, never in the checkout; `applied` means the base already carries the change and `old` was recovered by reversing it. `400`: `path` isn't a plain relative path or isn't in the diff, or a bad `side`. `404`: no diff, no checkout bound here, or that side doesn't exist (a new file has no `old`, a deleted one no `new`). `409`: the diff applies to neither base. `413` over 512 KB, `415` not UTF-8. The app uses it to render a changed Markdown file |
| GET | `/v1/proposals/{id}/comments` | `{comments: [Comment]}`, the refinement thread, oldest first |
| POST | `/v1/proposals/{id}/comments` `{text, requestChanges?}` | `{comment, proposal}`. `requestChanges: true` marks it a change request |
| POST | `/v1/proposals/{id}/request-changes` `{note?}` | `{proposal, message}`. The desktop's "Send to agent to revise"; `note` is added first as a change request |
| GET | `/v1/proposal-sets?status=open\|all` | `{sets: [ProposalSet]}`, every [proposal set](../docs-site/docs/ligus/features/proposal-sets.md) in the workspace, newest first (open by default). Read-only |
| GET | `/v1/proposal-sets/{id}` | `{set: ProposalSet}` |
| GET | `/v1/chats/{id}/proposal-sets?status=open\|all` | `{sets: [ProposalSet]}`, one chat's sets (all by default) |
| GET | `/v1/guards` | `{dearth: {reason, broodPaths, allowedMcpServers, setBy, setAt}?, pendingMembers, lockedChats: [{chatId, chatTitle, signals: [{signal, detail, raisedAt, messageId?}]}], coordinator: {holder, epoch, leaseMs, released, isThis}?, myRole, canClearLockdown}` |
| POST | `/v1/chats/{id}/lockdown/clear` | `{chatId, cleared: true}`. Writes the same signed `ChatLockdownCleared` the desktop's *Clear lockdown* does. `403` unless this box's identity is a human member at Contributor or above; `409` when the chat isn't locked |
| GET | `/v1/flow` | `{board: [BoardItem], queue: Queue, marks: [Mark], routing: Routing}`, the desktop's Flow pane, read-only; see below |
| GET | `/v1/chats/{id}/handoffs` | `{handoffs: [Handoff]}`, oldest first |
| GET | `/v1/dashboard` | `{title, source, refreshSeconds, sections: [{title, panels: [Panel]}], updatedAt, error?}`. Defined by the host's `dashboard.toml`; see [dashboard.md](dashboard.md) |
| GET | `/v1/appearance` | `{palette, mode, source}`, the host's theme for a phone set to "Match host". `palette` is `pollen`, `studio`, `harbor`, `slate`, `meadow`, `obsidian` or `null`; `mode` is `auto`, `light`, `dark` or `null`; `source` is `config`, `desktop` or `none` |
| GET | `/v1/lab/status` | **Deprecated** (`Deprecation: true`; use `/v1/dashboard`). `{workers: [{name, up}], powerWatts?, costPerHour?, ratePerKwh?, temps: [{name, celsius}], solar?, updatedAt}` |
| GET | `/v1/events` | `text/event-stream` |

`Message = {id, chatId, author, role: "user"|"agent"|"assistant"|"system", agentId?, actorId?, via?, body, createdAt, streaming: bool}`.
`actorId` (on a person's message) matches `Person.actorId`. A message the box
wrote as its own member before it was enrolled as the owner's device reads as
the owner's in the message list: `author` "Alex via Phone", `actorId` the
owner's, `via` the old name.
`role` is `agent` for a named agent's reply, which also carries `agentId`, and
`assistant` for the chat's primary runtime. `kind` is `fileDiff`, `command` or
`decision`. `status` is `open`, `approved`, `rejected` or `applied`. `diff` is
a unified diff.

Every proposal also carries its collaboration state, as the desktop's Review
card shows it: `requiredApprovals`, `qualifyingApprovals`, `quorumMet`,
`approvalRoleFloor`, `votes: [{actorId, voter, role, approved, createdAt}]`
(one per member, latest wins), `comments: [Comment]`, `commentCount`,
`changeRequestCount`, `changedFiles`, `dismissed`, `parentId?` (the version it
revises) and `supersededBy?` (its newer version). Proposals in API responses
and events also say what this box's identity, the one the phone acts as, can
do: `myRole`, `myVote: "approve"|"reject"|null`, `canVote` (its role meets the
content floor the core applies to votes and notes) and `isMine`.
`Comment = {id, actorId, author, role, text, requestChanges, createdAt}`.

`ProposalSet = {id, chatId, chatTitle, question, body, author, taskClass?,
phase: "scouting"|"piping"|"decided", open, round, attempt, agentQuorum,
qualityBar, decayPerRound, requiredApprovals, approvalRoleFloor, dial:
"fast"|"balanced"|"careful"|"custom", scoutingDeadline?, deadlinePassed,
frontRunner?, tieBreak, winner?, decidedAt?, createdAt, alternatives:
[Alternative], decideOn: "desktop"}`.
`Alternative = {proposalId, label: "A"|"B"|…, title, body, kind, status,
author, changedFiles, support, qualifyingScouts, scouts, runtimes, meetsQuorum,
frozen, active, superseded, isFrontRunner, isWinner, stopReason?:
"quorum"|"tieDeadline"|"withdrawn", stoppedBy?, assessments: [{id, scoutName,
scoutRuntime, score, currentSupport, rationale, round, isSelf, createdAt}],
qualifyingApprovals, requiredApprovals, votes, myVote, canVote, isMine,
reverted, preflight?: {passed, tests, baseUnchanged, hostsOnline, at}}`, each
check `{status: "passed"|"failed"|"skipped"|"unknown", note}`. `support` is
the decayed sum over counting scouts; an assessment by the alternative's own
producer is listed with `isSelf` and never counts. Assessments are blind until
posted: only posted ones exist, so a scout in flight is never shown. Vote on an
alternative with `POST /v1/proposals/{proposalId}/approve|reject`; tuning,
pre-flight, apply and the decision are desktop-only (`decideOn`).

`BoardItem = {id, kind: "failingTests"|"unreviewedProposal"|"failedStage"|"needsReview",
kindLabel, title, chatId?, chatTitle?, proposalId?, sinceMs, stimulus, claimedBy}`.
`Queue = {gate: {open, oldestWaitMs, medianWaitMs, trembleShare,
openedLastHour, decidedLastHour, overLimit}, edges: [{edge, samples,
medianWaitMs, trembleShare}], alert?, queueLimit, longWaitMs, recruits: [{id,
action, queueLen, oldestWaitMs, memberCount, chatId, atMs}]}` (the default
thresholds: queue limit 5, long wait 2 h). `Mark = {id, kind:
"claim"|"underConstruction"|"needsReview", kindLabel, path, lines?: [a, b],
taskId, note, by, byAgent, atMs, expiresMs?, describe, chatId?, chatTitle?}`.
`Routing = {windowDays: 14, decisions, liveDecisions, offPrimaryShare,
explored, topPicks: [{agent, name, count}], arms: [{arm, turns, proposals,
accepted, judged, acceptanceRate?}], stops: [{route, name, reason, untilMs,
failures}]}`. `Handoff = {id, from, to, fromName, toName, task, taskClass,
branch, files, via: "mention"|"stage", quality: {testsPassed?, proposalId?,
proposalStatus, turns, costUsd, error}, atMs, messageId?, parcel?: {keyFiles,
decisions, ruledOut, notes}, replyMessageId?}`; `messageId` is the reply that
handed the work on, so the app can place the card in the transcript, and
`replyMessageId` is the receiver's answer once it has replied. A lockdown
signal's `messageId` is the reply whose turn raised it (absent for a manual
lockdown).

A workflow run in `GET /v1/chats/{id}/details` also carries `coordinator:
{holder, holderLabel, epoch, released, isThis, online}?`, the device driving it
(its run lease), or `null`.
`role` everywhere is `owner`, `admin`, `contributor` or `viewer`.

Field notes:

- `unread` counts messages after the phone's read mark that this box didn't
  write. System notes don't count. Fetching messages moves the mark to the last
  message returned. On startup every existing chat is marked read.
- `preview` is the newest non-empty, non-system message, up to 140 characters.
- `/v1/proposals?status=open` lists open proposals that haven't been dismissed
  or superseded. `all` lists every proposal.
- Votes and notes go through the same core calls as the desktop
  (`ProposalVoteCast` and `ProposalRefined` deltas, signed by this box at its
  real roster role), so they sync to every device and merge with concurrent
  ones. A role below the content floor gets `403` and nothing is written.
- `request-changes` bundles the thread's change requests (all notes if there
  are none; `400` if the thread is empty), records "Sent to the agent to
  revise." on the thread and posts the instruction as an `@agent` message to
  the agent that produced the proposal, so whichever device hosts it answers.
  When that agent files its next proposal in the chat, this box links it
  (`parentId`) and marks the original `supersededBy`, and the original leaves
  the open list. `409` if the proposal is already revised or applied, or if it
  came from the chat's primary runtime and no `LIGUS_SERVE_DEFAULT_AGENT` names
  an agent to send it to.
- An agent with no host runs on its owner's device. It has no `hostId`, and its
  `hostLabel` is "Owner's device". For an agent on this box, `hostLabel` is
  "<label> (this host)".
- Avatars: `avatarUrl` is the picture a member or agent uploaded on the
  desktop, a bounded `data:image/…` URL (about 96 KB at most) that rides the
  synced roster. Without one, draw initials on `avatarColorHex` (agents) or a
  color derived from the name. Avatars are sent only by `/v1/people`,
  `/v1/agents` and chat details, never on chat rows, so a client caches them
  by id.
- `hostKind` says where an agent runs, as the desktop's host badge does:
  `ownerDevice` (no host: its owner's machine), `thisHost` (this box),
  `worker` or `device` (another registered host). `hostOnline` is present
  for an agent with a host.
- `Person = {actorId, displayName, kind, role: "owner"|"admin"|"contributor"|"viewer", title?, avatarUrl?, isSelf: bool}`.
  `isSelf` marks this box's own identity (the one the phone writes as).
- `channelId` files a chat under a channel (`""` = unfiled). A channel's
  `defaultChatId` is its drop-in chat. `POST /v1/chats` files the new chat
  under the first channel, as the desktop does.
- `Participant = {kind: "person"|"agent"|"primary", id, name}`: who spoke in the
  chat, newest first, at most 5. `id` is an actor id (`person`), an agent id
  (`agent`) or `hive` (`primary`, the chat's own `@hive`).
- `GET /v1/chats/{id}/details` answers
  `{chat, channel?, people: [Person + inChat], agents: [Agent + inChat], defaultResponder, context, workflowRuns, skills, vaults}`.
  `inChat` is true for whoever spoke there (or an agent added to the chat).
  `defaultResponder = {kind: "primary"|"agent", name, via: "untagged"|"serveDefault", runtimeLabel?, model?, agentId?, hostLabel?, hostKind?}`
  is who answers an untagged message: the chat's `@hive` on its runtime, or the
  agent named by `ligus serve --default-agent`.
  For the primary it also carries `available?: bool`, `availableVia?: string`
  and `unavailableReason?: "desktopOffline"|"noDesktop"`.
  Only a desktop runs `@hive`: its owner's (the chat's creator; once this box
  is enrolled as the owner's device, chats the phone creates are the owner's).
  `available: true` with `availableVia` = that desktop's name while it
  heartbeats its Device host; `false` with `desktopOffline` when it's closed
  (the turn waits and is answered once it opens), or `noDesktop` when the chat
  is this box's own and no desktop will run it (enroll the box as your device);
  absent when unknown (a desktop too old to heartbeat).
  `context = {estimated: true, windowTokens, usedTokens, historyTokens, skillTokens, messageCount, compacting, model?}`
  is a coarse estimate from the transcript, the loaded skills and the model's
  window (the desktop's Context pane plans the real prompt).
  `workflowRuns = [{id, name, status: "running"|"awaitingGate"|"completed"|"failed"|"halted"|"canceled", startedAt, updatedAt, stepsDone, stepsTotal, initiator?}]`,
  newest first, at most 10. `skills = [{id, name, summary, sourceUrl?, agentIds?}]`
  and `vaults = [{label, url, agentIds?}]` (`agentIds` absent = every agent).
- `/v1/dashboard` is a built-in "Hive" section (workspace metrics: agents,
  proposals, workflow runs, turns, this host's status) plus what the host
  owner configures in `dashboard.toml` in the data dir (format, panel kinds,
  metrics and examples in [dashboard.md](dashboard.md)).
  `Panel = {id, title, kind: "stat"|"gauge"|"timeseries"|"bar"|"list"|"status"|"text"|"table", unit?, value?, text?, level?: "ok"|"warn"|"critical", sparkline?: [{t, v}], series?: [{name, points: [{t, v}]}], rows?: [{label, value?, text?, up?, level?}], columns?: [{label, align?: "leading"|"center"|"trailing"}], cells?: [[string|number|bool|null]], min?, max?, thresholds?: [{value, level}], refreshSeconds, updatedAt, error?}`.
  The host evaluates every panel (a workspace metric, Prometheus, a JSON URL
  on its network, or an MCP tool the file allow-lists) with a per-panel
  timeout and cache; a failing panel carries `error` and the rest still
  render. With no file, a host serves the Hive section, plus a generated lab
  dashboard (the old `/v1/lab/status` content) below it when
  `LIGUS_PROMETHEUS_URL` is set. Only `builtin = false` with no sections of
  its own (or a host older than the built-in section) gives an empty one
  (`sections: []`). The app hides its dashboard tab when the dashboard is
  empty, and falls back to `/v1/lab/status` only when `/v1/dashboard` is 404
  (an older host).
- `/v1/appearance` resolves each field on its own: serve config first
  (`LIGUS_SERVE_PALETTE`, `LIGUS_SERVE_APPEARANCE`), then the desktop app's
  `appearance.json` on the same machine (`LIGUS_DESKTOP_DATA_DIR`, default the
  OS app-data dir of `ai.ligus.desktop`; a Hive 1.x `com.hive.desktop` dir is
  never read), which the desktop rewrites whenever
  its theme changes. The file is read per request. A client treats `null` (or a
  404 from an older server) as "use the default palette".
- `/v1/lab/status` (deprecated) is best-effort. Prometheus (`LIGUS_PROMETHEUS_URL`, default
  `http://127.0.0.1:9090`) gets each query with a 2 s timeout. A field whose
  query fails is left out. `workers` comes from `rdnagw_worker_up{worker}`,
  power and cost from `lab:power_watts:sum`, `lab:cost_usd_per_hour` and
  `lab:electric_rate_usd_per_kwh`, and temps from `LIGUS_LAB_TEMPS`
  (`Name=promql;…`).

### Repos (`/v1/repos`, `/v1/chats/{id}/repo`, `/v1/chats/{id}/pr`)

These act on the box's own device-local state: the checkouts under
`LIGUS_REPOS_DIR` and `repo-bindings.toml`. Nothing here is synced. GitHub
access is the box's own `gh` login (`gh auth login` on the host). Hive never
stores or sends a token. Writes need the bearer token (or the tunnel), a box
that can write (not revoked or pending), and a roster role of at least
Contributor, and answer 403 otherwise. No path on the box is ever returned:
checkouts are named, and an absolute binding shows only its last component.

| Method | Path | Response |
|---|---|---|
| GET | `/v1/repos?q=&owner=&page=&perPage=` | `{local: [Checkout], github: GitHub, workspaceDefault?, jobs: [Job]}`. `q` filters both lists (GitHub search when signed in); `owner` lists one account or org; `page` from 1, `perPage` 1-100 (default 30) |
| GET | `/v1/repos/fork?repo=owner/name` | `{repo, login, fork: {fullName, checkout, cloned}|null}`: the signed-in account's fork of `repo` (same name under the login, reported by GitHub as a fork of exactly `repo`). The phone calls this before cloning a repo the user doesn't own, to offer "Clone via my fork". 412 when `gh` isn't signed in |
| POST | `/v1/repos/clone` `{repo, viaFork?}` | `{job}`. `repo` is `owner/name` or `https://github.com/owner/name[.git]`, nothing else (400). Clones to `<LIGUS_REPOS_DIR>/<owner>__<name>`; an existing clone of the same repo is fetched instead (`action: "fetch"`), and one already in flight returns its job. At most 2 at once (429). `viaFork: true` clones the account's fork of `repo` instead (`checkout` is `<login>__<name>`); with no fork it answers 404 and creates nothing. Whenever GitHub reports the cloned or fetched repo as a fork, the job adds an `upstream` remote for the parent (fetch-only: its push URL is set to `DISABLED-push-to-fork-only`), fetches it, and, on a fresh clone, makes the local base branch track `upstream/<parent default>` |
| GET | `/v1/repos/clone/{job}` | `{job}` |
| GET | `/v1/chats/{id}/repo` | `Binding` |
| PUT | `/v1/chats/{id}/repo` `{repo}` | `Binding`. `repo` is a checkout name (or `owner/name` of a clone); `null` drops the chat's own binding, so it falls back to the workspace default. 404 for a checkout that isn't there. A change posts a system note in the chat |
| POST | `/v1/chats/{id}/repo/pull` | `Binding + {updated, before, after}`. Fetches `origin` and fast-forwards the checked-out base branch. In a fork it also fetches `upstream` and fast-forwards the base branch (`fork.baseBranch`) from `upstream`, whether or not it's checked out. 409 if the checkout is dirty, detached or diverged |
| POST | `/v1/chats/{id}/pr` `{title, body?, draft?}` | `{url, existing, branch, pushedBranch, repo, base, draft}`. Not a fork: pushes `hive/chat/<id>` to `origin` (never forced) and runs `gh pr create` against the default branch (`draft` defaults to false). A fork: pushes the chat branch to `origin` (the fork) as `hive/<title-slug>-<chat-id-8>` (the name is recorded in `pr-branches.toml`, so later pushes update the same branch and PR), then `gh pr create --repo <upstream> --base <upstream default> --head <forkOwner>:<branch>`, a draft unless `draft: false`. Any push whose remote resolves to the upstream repository is refused (409). A fork checkout with no `upstream` remote yet answers 409 (fetch it from the picker first). The link is posted as a system note. 412 when `gh` isn't signed in on the host; 409 when the chat branch is missing, has nothing new, or the push is refused |

- `Checkout = {name, displayName, defaultBranch?, branch?, head?, dirty, remote: {host, owner, name, fullName}|null, fork?: Fork, isWorkspaceDefault, boundChats}`.
  `displayName` is `owner/name` for a hosted remote, else the name. `dirty`
  ignores the app's own `.ligus/` (and any Hive 1.x `.hive/`) state. In a fork, `defaultBranch` is
  upstream's default branch.
- `Fork = {upstream: "kubeflow/pipelines", upstreamRemote: "upstream", baseBranch, upstreamPushDisabled, prTarget: "upstream", behind?, ahead?}`,
  present when the checkout has an `upstream` remote pointing at another
  repository. `behind`/`ahead` compare the local base branch with
  `upstream/<baseBranch>` as of the last fetch. The phone shows it as
  "fork of kubeflow/pipelines · PRs open upstream". New chat branches in a
  fork start from `upstream/<baseBranch>` (the box keeps the checkout's
  `hive.chatBase` git config pointing there).
- `GitHub = {available, reason?, login?, owners?: [string], repos?: [{fullName, owner, name, description, private, defaultBranch, updatedAt, url, fork, cloned, checkout?, viaForkCheckout?}], page?, perPage?, hasMore?, error?}`.
  `viaForkCheckout` names a local fork checkout of that repository.
  Signed out it is `{available: false, reason: "Run gh auth login on the host"}`,
  and public repos can still be cloned.
- `Job = {id, repo, checkout, action: "clone"|"fetch", state: "running"|"done"|"error", phase, percent?, error?, startedAt, finishedAt?}`.
  Progress also arrives on the event stream as `repo.clone`. A clone lands in
  a hidden temp dir and is renamed into place when it succeeds, so a failed or
  stopped clone leaves nothing behind. Limits: `LIGUS_REPOS_CLONE_TIMEOUT_SECS`
  (default 900) and `LIGUS_REPOS_MAX_CLONE_MB` (default 2048). Git never
  prompts (`GIT_TERMINAL_PROMPT=0`); with `gh` signed in, git uses
  `gh auth git-credential` for github.com.
- `Binding = {repo: (Checkout + {source: "chat"|"workspace", available, external})|null, workspaceDefault?, chatBranch, chatBranchExists?, ahead?, behind?, prBranch?}`.
  `chatBranch` is `hive/chat/<id>`. Each repo has its own, so switching a chat
  away and back resumes where it was. `ahead`/`behind` compare it with the
  default branch (`upstream/<baseBranch>` in a fork). `prBranch` is the
  branch a fork PR from this chat was pushed as. `GET /v1/chats/{id}/details` carries the same object as
  `repo`.

Approved proposals reach `hive/chat/<id>` only when the box applies them
(`LIGUS_SERVE_APPLY_ON_APPROVE=1`). Otherwise the chat branch has nothing to
open a pull request for.

#### Code hosts (0.1, additive)

The rows above are unchanged for the shipped app; 0.1 adds:

- `GET /v1/repos` also returns `forges: [Forge]` (every connection on the
  box). With `&forge=<id>` it returns that connection's listing as `forge`
  (the `GitHub` shape plus `id`) instead of `github`.
- `GET /v1/repos/fork` and `POST /v1/repos/clone` take `forge` (default
  `github.com`); a `https://<host>/…` URL picks the connection by host (400 if
  the host has no connection). Checkouts from other hosts are named
  `<host>__<owner>__<name>`.
- `POST /v1/chats/{id}/pr` answers with `kind` (`pr` or `mr`) and `forge`.
  Drafts are a real draft on GitHub, a `Draft:` title on GitLab and a `WIP:`
  title on Forgejo, Gitea and Codeberg.
- `Forge = {id, kind, flavor, displayName, baseUrl, authSource: gh|token|oauth|none, user?, available, reason?, hasRead, hasWrite, deviceFlow, openLabel, changeRequestKind, holder}`.
- `GET|POST /v1/forges`, `DELETE /v1/forges/{id}`, `POST /v1/forges/{id}/test`,
  `PUT|DELETE /v1/forges/{id}/token`, `POST|GET /v1/forges/{id}/auth/device`:
  see `docs-site/docs/operate/reference/serve-api.md`. The phone never stores a forge
  token. `PUT …/token` and `POST …/auth/device` are accepted only through the
  tunnel (or on a loopback bind) and need presence; on a direct LAN bind
  they answer 403, and `GET /v1/forges` says `tokenEntry: false`.

#### Identity: safety codes, invites, contacts and DMs (0.1, additive)

The phone is a remote control of the box, and the box is one Ligus device:
everything below is the box's own account (or, on a box enrolled as its
owner's device, the owner's), its pinned config logs and its contact.
Every name is cleaned (`hive_proto::names`: bidi, zero-width, invisible
format and control characters removed); the app cleans them again. The
contract is also in `docs-site/docs/operate/reference/serve-api.md`.

**Invite codes travel only over a secure channel.** A code carries the
workspace passphrase and its epoch keys (a DM's, its room key), so, like a
code-host token, it goes only through the end-to-end encrypted tunnel or on a
loopback bind. On a direct LAN or tailnet bind (plain HTTP) the box answers
403 `{error}` with a message the app shows as is ("…open this over remote
access…") to `POST /v1/invites`, `POST /v1/presence/prepare {action:
"invite"}` (and a `commit` of an invite prepared elsewhere),
`POST /v1/invites/inspect`, `POST /v1/invites/join` and a `POST /v1/dms` that
would found a DM. Opening a kept DM, listing, and accepting or dismissing a
pending invite (by reference; the code stays on the box) work on any path.
The app sends these calls through the tunnel whenever the host is paired with
remote access, even while the direct path is in use; a host paired for the
direct path only shows the box's message. The API never terminates TLS
itself: https in front of it counts only when the box listens on loopback.

| Method | Path | Response |
|---|---|---|
| GET | `/v1/profile` | `{accountId, displayName, safetyCode?, holdsAccountKey, canFoundDm, contacts: on\|unsupported\|noRelay\|delegated\|unknown, avatarUrl?}`. `safetyCode` is eight groups of four hex characters (the account fingerprint, the invite's `fk`) |
| GET | `/v1/people` | Each person also carries `accountId`, `safetyCode` (absent while the shared logs disagree), `keyConflict`, `lookAlike` (another co-member's name, or this box's own, looks like theirs), `devices` (verified key-agreement keys), `links: [LinkChip]` and `dmChatId?` |
| GET | `/v1/people/{accountId}/links` | `{links: [LinkChip]}` for this box's own account or a co-member (404 otherwise). Cached statuses only: this route never asks a forge. `LinkChip = {provider, host, handle, subjectId, status?}`, `status` the desktop's `LinkStatus` (`verified`, `signatureOnly`, `stale`, `unreachable`, `expired`, `selfAsserted`, `notChecked`); `invalid` links are left out; handles and ids carry no hidden characters |
| POST | `/v1/invites` | `{code, signed, safetyCode?, workspaceName}`: a single-use invite to the box's workspace (an Admin's `JoinApproved`), signed by the founder's device when this box is the founder's (`ligus-invite-v1`). 403 `presence_required` on a box acting for its owner (interim gate) or a rules-v2 workspace: confirm with `POST /v1/presence/prepare {action: "invite"}` and `commit`, whose answer then carries `invite` |
| POST | `/v1/invites/inspect` `{code}` | `{workspaceName, isDm, addressedToMe, founderAccount?, founderName?, safetyCode?, trust, alreadyJoined, acceptable, refusal?}`. `trust` is the desktop's: `known` (a co-member's device signed it and their code matches), `claimed` (names a co-member and their code without their signature: only says it's from them), `mismatch`, `unknown` (compare the code out of band), `none`. Nothing is stored |
| POST | `/v1/invites/join` `{code, confirmSafetyCode}` | `{workspaceId, account}`: joins a DM invite under `hive_runtime::dm::accept_dm_invite` (addressed to this account, signed by the sender's device, key matching the shared logs; a stranger only with `confirmSafetyCode` after comparing codes; never a room the box holds). The relay must be the box's own or a public `https` one; another relay is reached only at public addresses, checked at each connect (a name re-pointed inside the network is refused), with no redirects. Behind an HTTP(S) proxy (which resolves the name itself) the box resolves that relay's name first, at join and before each sync or contact call, and refuses it when it resolves to a non-public address (a name only the proxy can resolve is left to the proxy). A workspace invite answers 409: a box serves one workspace |
| GET | `/v1/invites/pending` | `{contacts, invites: [{relay, seq, isDm, workspaceName, founderAccount?, founderName?, safetyCode?, trust, acceptable, refusal?}]}`: the box's contact drop box (relay v0.3), read and acknowledged like the desktop's. The codes stay on the box |
| POST | `/v1/invites/pending/accept` `{relay, seq, confirmSafetyCode}` | `{workspaceId, account}` (by reference to the last listing) |
| POST | `/v1/invites/pending/dismiss` `{relay, seq}` | `{dismissed: true}` |
| GET | `/v1/dms` | `{dms: [{workspaceId, account, name, chatId?, safetyCode?, avatarUrl?, archived, waitingForFounder, foundedAt?, updatedAt?}], canFound}` |
| POST | `/v1/dms` `{accountId}` | `{workspaceId, chatId?, invite?, delivered, safetyCode?}`: opens the kept DM with a co-member, or founds one (random `dm-` room and key, this box's account pinned as founder, a signed invite addressed to them, sealed to their verified devices and dropped into their contact box). `invite` (hand it over yourself) is set on creation; `delivered` says it reached their box. A box acting for its owner can't found one (409) |
| GET | `/v1/chats/{id}/founder` | `{waiting, founderName?}` |

- **DM chats** are listed by `GET /v1/chats` with `dm: {workspaceId, account,
  name, archived, safetyCode?}` (the title is the other person's name), and
  read and written through `/v1/chats/{id}/messages`. No other route takes a
  DM's chat. Agents never run in a DM, and DM messages never push (ntfy or
  relay).
- **Waiting for the founder.** A chat row carries `waitingForFounder: true`
  (and `founderName`) while writes into its workspace wait for the pinned
  founder's signed founding; a send then answers 409
  `{error: "waiting_for_founder", message, founderName?}`. A state, not a
  failure: keep the draft.
- Each DM syncs in its own loop on its own relay; the workspace's relay token
  and sign-in go only to the box's own relay. DMs are device-local
  (`serve-dms.json`, keys in the secret store) and kept by the
  foreign-workspace cleanup.
- The box publishes a contact record (relay v0.3) when it holds its account's
  key, announcing its id with one host beat when the log doesn't carry it.

### Events (`GET /v1/events`)

Each event has `id: <n>` (increasing), `event: <name>` and `data: <JSON>`. A
`: heartbeat` comment is sent every 15 s. `Last-Event-ID` replays buffered
events after that id, best effort: the server keeps the last 1024. A client
that falls further behind should refetch.

| `event:` | `data:` |
|---|---|
| `message.created` | `{message}` (an agent reply starts with `streaming: true` and an empty body) |
| `message.delta` | `{chatId, messageId, text}`, a streamed fragment of a reply produced on this box |
| `message.done` | `{message}`, the final body with `streaming: false`. Replies synced in already finished get `created` followed by `done` |
| `chat.updated` | `{chat}` (same shape as in `/v1/chats`) |
| `proposal.created` | `{proposal}` |
| `proposal.updated` | `{proposal}`, when a known proposal's status, votes, thread, revision link or dismissal changes (from this phone or any device) |
| `turn.status` | `{chatId, agentId, state: "queued"\|"running"\|"done"\|"error", detail?}` |
| `repo.clone` | `{job}`, a clone or fetch job's progress and its end (see "Repos") |
| `dms.changed` | `{}`: a DM was opened, joined or synced; refetch `/v1/dms` and `/v1/chats` |

### Pairing and links

`ligus serve --pair` prints a terminal QR code and the text link when it
starts. `hive pair` does the same without starting a server, which is handy
while the systemd unit runs. Run it with the server's settings
(`LIGUS_SERVE_LISTEN` or `LIGUS_SERVE_PUBLIC_URL`, and `LIGUS_DATA_DIR`): it
refuses to print a link to a loopback address, and it never creates a token of
its own, so it can't hand the phone one the server doesn't accept. The link is
`hive://pair?url=<base64url(serverURL), unpadded>&token=<token>`, where
`serverURL` is `LIGUS_SERVE_PUBLIC_URL` or `http://<listen>`. The app's URL
scheme is `hive`: `hive://pair?…` and `hive://chat/<id>`. ntfy notifications
carry `Click: hive://chat/<chatId>`.

### Remote access through a relay (optional, off by default)

`ligus serve` can also be reached without Tailscale, through a relay, over an
end-to-end encrypted and mutually authenticated tunnel (protocol:
[`relay-tunnel.md`](relay-tunnel.md), `hive-tunnel/1`). The host keeps one
outbound WebSocket to the relay and opens no inbound port. Each phone request
is dispatched into the same axum router in process, so the API and SSE
contract above is unchanged. The relay sees only ciphertext.

```sh
hive remote enable --relay https://relay.example.com [--reg-token T]
#   or per run: hive serve --remote-relay URL   (env LIGUS_REMOTE_RELAY;
#   LIGUS_REMOTE_RELAY=off overrides relay.json; LIGUS_REMOTE_REG_TOKEN)
hive pair                  # v2 link: url/token plus v=2&hid&hs&relay&ps&exp
hive pair --relay-only     # no url/token (also automatic when the direct URL is loopback)
hive remote status         # relay, host id, live connection, phone count
hive phones list
hive phones revoke <pid|name> [--keep-token]
hive remote disable | reset   # reset = new host keys: every phone re-pairs
```

- State is in `<LIGUS_DATA_DIR>/tunnel/` (directory 0700, files 0600):
  `keys.json` (Ed25519 signing key and X25519 Noise static), `phones.json`,
  `relay.json`, `status.json`, and `pending/` for the hand-off from `hive pair`
  and `hive phones revoke` to the running server (picked up within 2 s).
- The relay URL must be `https`; `--insecure-relay` /
  `LIGUS_REMOTE_INSECURE_RELAY=1` allows `http` (tests, a loopback relay).
- Each v2 link opens a one-time pairing window of 10 minutes. The phone gets a
  per-phone credential (`hpc1`, 90 days) and renews it daily through
  `POST /v1/tunnel/credential`. That route and `PUT /v1/tunnel/push
  {"enabled": bool}` exist only through the tunnel and return 404 with a
  bearer token.
- End-to-end encrypted push notifications (replies, mentions, reviews,
  workflow approvals) and how to enable them with your own APNs key:
  [`push-notifications.md`](push-notifications.md).
- `hive phones revoke` takes effect at once: the server ends the phone's live
  sessions (GOAWAY), refuses its requests and handshakes, and tells the relay
  (retried until acked). It also rotates `serve.token` unless `--keep-token`,
  because a stolen phone holds that too. A running server picks up the new
  token within 2 s.
- Through the tunnel, requests carry no bearer token: the phone is
  authenticated by its Noise static key, and every request re-checks that the
  phone is still paired.

## Security model

- **Tailnet only.** The API listens on loopback, a private LAN address or the
  box's Tailscale IP. Unspecified and public binds are refused unless
  overridden. Tailscale ACLs decide which devices can reach the port. The
  transport is plain HTTP inside WireGuard: the tailnet supplies confidentiality
  and peer authentication, and the bearer token supplies authorization.
- **Bearer token.** 32 random bytes, hex-encoded, in
  `<LIGUS_DATA_DIR>/serve.token` (created with mode 0600 on first run). It's
  compared in constant time. The phone receives it once, through the pairing
  link, and keeps it in the iOS Keychain
  (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, not synced to iCloud).
  To revoke it, delete the file and restart, then pair again. The token grants
  what this box's workspace member can do, and nothing more.
- **Keys never leave the box.** The workspace E2EE key (from the
  `hivews1:` invite, used with `ligus enroll`) and the device and account
  signing keys stay in `LIGUS_DATA_DIR`. The relay sees only ciphertext. The
  phone sees only plaintext API responses over the tailnet, and nothing it
  holds can decrypt the relay's log or sign workspace events.
- **The box is its own member.** It joins the roster as **Contributor** (self
  enrollment) and signs every event it writes, including the phone's messages,
  which appear under `LIGUS_SERVE_NAME`, and the phone's votes. Peers verify
  those events on ingest like any other device's. Approve and reject cast the
  same `ProposalVoteCast` delta the desktop's Review pane does, at this
  member's real roster role. Quorum, the role floor and the no-self-approval
  rule apply unchanged, so a proposal whose floor is above this member's role
  needs another reviewer or a promotion. Agent proposals are authorless, as
  on the desktop.
- **Tool policy.** The desktop's defaults apply:
  - Built-in file tools are offered only to runtimes that may front a local
    server (Custom, HiveDaemon, Ollama) and only when the endpoint is on this
    machine, a private network or the tailnet, unless
    `LIGUS_ALLOW_REMOTE_FILE_TOOLS=1`. Hosted APIs never get the tool loop.
  - Reads come from the chat's base worktree. **Writes only go to a per-turn
    git worktree**, cut on the first write, and its diff becomes a
    review-gated `fileDiff` proposal. The checkout is never written in place.
    Without a bound checkout a turn gets no file tools.
  - `run_command` is off unless `LIGUS_BUILTIN_RUN_COMMAND=1`, and even then
    it runs only inside the turn's worktree.
  - **Exfiltration guard.** Once a turn has read project files, MCP calls are
    refused for the rest of the turn. Only `LIGUS_MCP_AFTER_READS=1` turns
    this off. MCP results are labelled as untrusted data.
  - **Role gate.** Tools of either kind run only when the triggering
    message's author has at least `LIGUS_MCP_MIN_ROLE` (default Contributor).
  - Optional: `LIGUS_SERVE_APPLY_ON_APPROVE=1` applies an approved diff to the
    chat's branch (`hive/chat/<id>`) in the bound checkout, the way the
    desktop's Implement does. Only a proposal whose status is `approved`
    applies: one a qualifying reviewer rejected, or that was dismissed or
    superseded, never does.
  - A file-writing subprocess agent (aider, pi, codex, hermes, or Claude Code
    in an edit mode) runs only in a per-turn worktree of a bound repository.
    Without one the turn fails instead of writing in place.
- **Notifications carry no content.** An ntfy push says who replied in which
  chat, or that a change is ready for review, plus the deep link. It never
  includes message bodies, diffs or secrets. `LIGUS_NTFY_TOKEN` (optional) is
  sent as a bearer token.
- **Secrets on the box** sit in the systemd `EnvironmentFile` (0600) and
  `config.toml`. Nothing is committed, and the pairing link is printed only on
  request.

## Limits

- Single user, single workspace per `ligus serve` process, and one phone per
  token (the token isn't per-device). Phones paired over the relay are
  per-device and can be revoked one by one.
- **Your own device.** With `LIGUS_SERVE_OWNER` set, the box asks to become one
  of the owner's devices: it prints an enrollment code (`hivedev1:…`, also
  `ligus serve enroll`), and the owner approves it on their desktop (Settings >
  Account > My devices > Add a device), checking the fingerprint. The desktop
  signs a certificate for the box's device key with the owner's account key,
  which never leaves the desktop. From then on the box authors everything as the
  owner (`identity.mode = "owner"`): messages, votes, comments and chats the
  phone creates are the owner's, so one person has one vote and the owner's
  desktop answers `@hive`. A box can't make itself the owner's device: an event
  it stamps as the owner without that certificate is quarantined everywhere as
  impersonation. Approving also removes the box's separate member (if it had
  one); its past messages show on the desktop as "<owner> via <box>".
  Revoking it (My devices > Revoke) stops every updated desktop accepting its
  events and makes the box read-only (`revoked`); `ligus serve enroll
  --new-device` makes a new device key to approve again. No new event type:
  older desktops accept the enrolled box's events as the owner's too, but only
  updated desktops keep honoring a revocation after the sync batch it arrived in.
- Before enrollment (no `LIGUS_SERVE_OWNER`, or not approved yet) the box is a
  separate member identity: the desktop shows the phone's messages under
  `LIGUS_SERVE_NAME`. A box that is already a member with `LIGUS_SERVE_OWNER` set
  (and an Admin) keeps the earlier stopgap meanwhile: desktops count it as one
  of the owner's devices for dispatch only. A box that isn't a member waits
  read-only (`pending`) instead of joining as one.
- Agents run one at a time on the worker thread. A long turn delays the next
  one, but the API and sync keep running.
- Deltas are pushed only for turns that run on this box. A reply streamed on
  another device arrives as `created` and then `done` once it syncs (3 s
  cadence).
- Event replay keeps 1024 events in memory and is lost on restart. The app
  refetches its lists and the open chat whenever its stream reconnects.
- On SIGTERM a turn in flight gets 15 s to finish. A reply cut off by a stop
  is finished on the next start with an "interrupted" note; ask again to
  retry.
- A fresh box whose first relay sync fails refuses to start (systemd
  retries), so it never seeds a workspace config of its own.
- Read marks are per box, not per device, and persist in
  `serve-state.json`.
- Creating agents and editing runtimes aren't in the API. Assigning existing
  agents to hosts is.
- New agents created on the desktop default to their owner's device. Send them
  here explicitly. Once an agent is sent to a host, only that host answers its
  mentions; the desktop defers to it (and answers after 25 s only if the host
  is offline).
- A pending "send to agent to revise" is remembered in memory: if the box
  restarts before the revision arrives, the new version isn't linked to the
  old one (both stay listed).
- Approving doesn't apply a diff by default; the desktop's Implement does that.
  Set `LIGUS_SERVE_APPLY_ON_APPROVE=1` to apply on the home host.

## Running and testing

```sh
hive serve --listen 100.x.y.z:8787 --label sff --pair
hive pair                                     # show the pairing QR code again
cargo test -p hive-cli                        # unit + integration (test relay, fake model, fake ntfy)
LIGUS_TUNNEL_INTEROP_RELAY=http://127.0.0.1:8443 \
  cargo test -p hive-cli interop_against_a_real_relay   # tunnel against a relay run with LIGUS_RELAY_TUNNEL=open
LIGUS_SERVE_LIVE_ENDPOINT=http://<gateway>:8080/v1/chat/completions \
LIGUS_SERVE_LIVE_MODELS=gpt-oss:20b,gemma4:12b \
  cargo test -p hive-cli serve::tests::live -- --nocapture
```

The integration tests start a real `ligus serve` against the in-process test
relay (`crates/hive-runtime/src/test_relay.rs`), with a desktop identity that
owns the workspace. They cover auth (401s), host assignment, a phone message
that reaches a hosted agent, the streamed `message.delta` and `message.done`,
a `write_file` tool call that becomes a `fileDiff` proposal, approval (also
seen by the desktop after sync), rejection with a reason, `Last-Event-ID`
replay, ntfy pushes, and the pairing link format. The tunnel tests check the
`relay-tunnel/vectors.json` vectors, the mux and flow control, and run an end
to end pairing, GET, SSE stream, renewal and revocation through a fake relay
and a fake phone. The live test runs one turn
per model against the real gateway, in which the agent has to `read_file` a
planted note to answer.

Deployment: [`deploy/ligus-serve/README.md`](../deploy/ligus-serve/README.md).

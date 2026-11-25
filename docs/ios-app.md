# Ligus on iPhone: thin client + home host

> The iPhone app's source now lives in a separate repository. This document
> stays here as the design and **API contract of `ligus serve`**, which any
> client can implement.

A single-user iOS app for one person's Ligus workspace. The
phone doesn't run Ligus. It's a thin client over Tailscale to **`ligus serve`**,
a home-host process on an always-on box in the lab. The home host is a normal
Ligus device in the workspace: it keeps the E2EE keys, syncs with the relay,
and runs agent turns on the lab's model gateway. The phone gets a small JSON
API plus a Server-Sent Events stream.

```
 iPhone (Ligus app)                         always-on box (tailnet)                     elsewhere
 ─────────────────                         ──────────────────────────────────────      ─────────
  SwiftUI client  ── HTTP+SSE, bearer ──▶  ligus serve                                  relay (E2EE
  token in Keychain    (tailnet only)      ├─ API + SSE   (crates/ligus-cli/src/serve)   opaque blobs)
                                           ├─ sync loop   ── push/pull envelopes ──▶      ▲
  ntfy app  ◀── push (Click: ligus://…) ──  ├─ agent worker (crates/ligus-cli/src/worker)    │
                                           │   └─ tool loop ── OpenAI wire ──▶ llama.cpp   desktop app
                                           │      (built-in file tools + MCP)   gateway    (same workspace)
                                           ├─ ntfy notifier ──▶ self-hosted ntfy
                                           └─ /v1/dashboard ──▶ workspace metrics, Prometheus, JSON URLs, MCP tools (host side)
```

## Components

- **`ligus serve`** (`crates/ligus-cli/src/serve/`) runs in one process:
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
  `dispatch::stream_with_tools` with `CompositeTools` (Ligus's built-in file
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
| GET | `/v1/me` | Tunnel or loopback only. `{workspaceId, room, workspaceName, configLogId, device: {id, fingerprint, tier: "phone"}, account: {id, memberId, displayName, role}, rpId, origins}`: who this phone is here (see "Add this phone" and "Add a phone by scanning the desktop's QR" below). `403 {error: "device_not_certified", message}` until the box knows the phone's certificate. `workspaceName` is the workspace config log's title, which the Owner's or an Admin's desktop writes once from the name it keeps in its settings (empty until then; the app then shows the box's name). Only an Owner or Admin can set it (every fold drops anyone else's); cleaned and at most 120 characters |
| POST | `/v1/phone-enroll/check` `{workspaceId, desktopDeviceId, rid, exp, deviceId?}` | 0.1.1. Who showed a scanned `ligusenroll1:` code: `{workspaceId, workspaceName, person: {accountId, memberId, displayName, role, nameCollision}, desktopLabel, phone: {standing: "new"\|"alreadyMine"}}`. See "Add a phone by scanning the desktop's QR" below |
| POST | `/v1/phone-enroll/requests` `{request}` | 0.1.1. `202 {rid, state: "relayed"}`: the box passed the phone's `ligusenrollreq1:` request to the desktop |
| GET | `/v1/phone-enroll/requests/{rid}?wait=N` | 0.1.1. `{state: "waiting"\|"answered"\|"expired", answer?, certified}`, long poll up to 25 s |
| GET | `/v1/health` | `{ok: true, version, workspace?, deviceLabel?, identity?}`. `workspace` (room name), `deviceLabel` and `identity` appear only when authorized. `identity = {mode: "member"\|"revoked", actorId, displayName, deviceFingerprint}`: the box's own service-member identity (`"owner"` is never reached: a box enrolled as a person's device refuses to start) |
| GET | `/v1/chats` | `{chats: [{id, title, updatedAt, preview, unread: int, channelId, participants: [Participant]}]}`, newest first |
| POST | `/v1/chats` `{title?}` | `{chat}` |
| GET | `/v1/chats/{id}/messages?after=<messageId>&limit=<n>` | `{messages: [Message]}`, oldest first. Without `after`, the latest page (default limit 100, max 1000) |
| POST | `/v1/chats/{id}/messages` `{body}` | `{message}`. `@Name` mentions route to agents exactly as on the desktop/CLI |
| GET | `/v1/chats/{id}/details` | The chat's details (the desktop's right-hand panel, read-only); see below |
| GET | `/v1/agents` | `{agents: [{id, name, runtimeLabel, model?, hostId?, hostLabel?, hostKind, hostOnline?, role?, avatarUrl?, avatarColorHex?, ownerActorId?, available, unavailableReason?, access}]}`. `ownerActorId` (absent when the agent has no recorded owner) is the account that may grant the agent's permission requests besides the host's own person: show the Allow button on a `permission` ask only when it is yours. `available` is whether this box's person may use the agent; when false, grey it out (no @mention) and show `unavailableReason` ("Only its owner can use this agent", "Only workspace Admins can use this agent"). `access` is `owner` or `everyone` ([agent access](../docs-site/docs/ligus/features/agent-access.md)); budgets aren't checked here |
| GET | `/v1/chats/{id}/asks` | `{asks: [Ask]}`, newest first. `Ask = {id, chatId, turnId, agentKey, agentName, kind, question, options, expiresAtMs, closed, open, answer?, answeredBy?, postedBy?}`; `kind` is `question` (an agent's `ligus_ask_human`) or `permission` (an ACP permission escalation the turn's host posted); decide on `kind`, never on the question's text. `postedBy` (0.3.1) names whoever posted an ask that isn't the agent's or its host's: show it as theirs; its `kind` is always `question`. `ask.pending` events carry the same `Ask` |
| GET | `/v1/people` | `{people: [Person]}`, the workspace's human members |
| GET | `/v1/channels` | `{channels: [{id, name, purpose?, position, archived, defaultChatId}]}`, in sidebar order |
| GET | `/v1/hosts` | `{hosts: [{id, label, isThis: bool, online: bool}]}` |
| PUT | `/v1/agents/{id}/host` `{hostId}` | `{agent}`. `hostId` must be a registered host. `""` hands the agent back to its owner's device |
| GET | `/v1/proposals?status=open\|all` | `{proposals: [{id, chatId, title, body, kind, diff?, status, createdAt, author}]}` |
| POST | `/v1/proposals/{id}/approve`, `/reject` | **410 Gone** `{error: "gone", message, use}`. Vote with the `vote` phone action (below) |
| GET | `/v1/proposals/{id}` | `{proposal}` |
| GET | `/v1/proposals/{id}/file?path=&side=old\|new` | `{path, side, content, size, base, applied}`: one file of the proposal's diff, whole, before (`old`) or after (`new`, the default) the change. Read from the chat's bound checkout (branch `ligus/chat/<id>`, else `HEAD`) with the file's patch applied in a scratch directory, never in the checkout; `applied` means the base already carries the change and `old` was recovered by reversing it. `400`: `path` isn't a plain relative path or isn't in the diff, or a bad `side`. `404`: no diff, no checkout bound here, or that side doesn't exist (a new file has no `old`, a deleted one no `new`). `409`: the diff applies to neither base. `413` over 512 KB, `415` not UTF-8. The app uses it to render a changed Markdown file |
| GET | `/v1/proposals/{id}/comments` | `{comments: [Comment]}`, the refinement thread, oldest first |
| POST | `/v1/proposals/{id}/comments` `{text, requestChanges?}` | `{comment, proposal}`. `requestChanges: true` marks it a change request |
| POST | `/v1/proposals/{id}/request-changes` `{note?}` | `{proposal, message}`. The desktop's "Send to agent to revise"; `note` is added first as a change request |
| GET | `/v1/proposal-sets?status=open\|all` | `{sets: [ProposalSet]}`, every [proposal set](../docs-site/docs/ligus/features/proposal-sets.md) in the workspace, newest first (open by default). Read-only |
| GET | `/v1/proposal-sets/{id}` | `{set: ProposalSet}` |
| GET | `/v1/chats/{id}/proposal-sets?status=open\|all` | `{sets: [ProposalSet]}`, one chat's sets (all by default) |
| GET | `/v1/guards` | `{dearth: {reason, broodPaths, allowedMcpServers, setBy, setAt}?, pendingMembers, lockedChats: [{chatId, chatTitle, signals: [{signal, detail, raisedAt, messageId?, raiseEventId}]}], coordinator: {holder, epoch, leaseMs, released, isThis}?, myRole, canClearLockdown}` |
| POST | `/v1/chats/{id}/lockdown/clear` | **410 Gone**. Clear with the `clearLockdown` phone action (below) |
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
alternative with the `vote` phone action; tuning,
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
  (`agent`) or `ligus` (`primary`, the chat's own `@ligus`).
- `GET /v1/chats/{id}/details` answers
  `{chat, channel?, people: [Person + inChat], agents: [Agent + inChat], defaultResponder, context, workflowRuns, skills, vaults}`.
  `inChat` is true for whoever spoke there (or an agent added to the chat).
  `defaultResponder = {kind: "primary"|"agent", name, via: "untagged"|"serveDefault", runtimeLabel?, model?, agentId?, hostLabel?, hostKind?}`
  is who answers an untagged message: the chat's `@ligus` on its runtime, or the
  agent named by `LIGUS_SERVE_DEFAULT_AGENT`.
  For the primary it also carries `available?: bool`, `availableVia?: string`
  and `unavailableReason?: "desktopOffline"|"noDesktop"`.
  Only a desktop runs `@ligus`: its owner's (the chat's creator; chats the
  phone creates are the box's own).
  `available: true` with `availableVia` = that desktop's name while it
  heartbeats its Device host; `false` with `desktopOffline` when it's closed
  (the turn waits and is answered once it opens), or `noDesktop` when the chat
  is this box's own and no desktop will run it (mention an agent, or set
  `LIGUS_SERVE_DEFAULT_AGENT`);
  absent when unknown (a desktop too old to heartbeat).
  `context = {estimated: true, windowTokens, usedTokens, historyTokens, skillTokens, messageCount, compacting, model?}`
  is a coarse estimate from the transcript, the loaded skills and the model's
  window (the desktop's Context pane plans the real prompt).
  `workflowRuns = [{id, name, status: "running"|"awaitingGate"|"completed"|"failed"|"halted"|"canceled", startedAt, updatedAt, stepsDone, stepsTotal, initiator?}]`,
  newest first, at most 10. `skills = [{id, name, summary, sourceUrl?, agentIds?}]`
  and `vaults = [{label, url, agentIds?}]` (`agentIds` absent = every agent).
- `/v1/dashboard` is a built-in "Ligus" section (workspace metrics: agents,
  proposals, workflow runs, turns, this host's status) plus what the host
  owner configures in `dashboard.toml` in the data dir (format, panel kinds,
  metrics and examples in [dashboard.md](dashboard.md)).
  `Panel = {id, title, kind: "stat"|"gauge"|"timeseries"|"bar"|"list"|"status"|"text"|"table", unit?, value?, text?, level?: "ok"|"warn"|"critical", sparkline?: [{t, v}], series?: [{name, points: [{t, v}]}], rows?: [{label, value?, text?, up?, level?}], columns?: [{label, align?: "leading"|"center"|"trailing"}], cells?: [[string|number|bool|null]], min?, max?, thresholds?: [{value, level}], refreshSeconds, updatedAt, error?}`.
  The host evaluates every panel (a workspace metric, Prometheus, a JSON URL
  on its network, or an MCP tool the file allow-lists) with a per-panel
  timeout and cache; a failing panel carries `error` and the rest still
  render. With no file, a host serves the Ligus section, plus a generated lab
  dashboard (the old `/v1/lab/status` content) below it when
  `LIGUS_PROMETHEUS_URL` is set. Only `builtin = false` with no sections of
  its own (or a host older than the built-in section) gives an empty one
  (`sections: []`). The app hides its dashboard tab when the dashboard is
  empty, and falls back to `/v1/lab/status` only when `/v1/dashboard` is 404
  (an older host).
- `/v1/appearance` resolves each field on its own: serve config first
  (`LIGUS_SERVE_PALETTE`, `LIGUS_SERVE_APPEARANCE`), then the desktop app's
  `appearance.json` on the same machine (`LIGUS_DESKTOP_DATA_DIR`, default the
  OS app-data dir of `ai.ligus.desktop`), which the desktop rewrites whenever
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
access is the box's own `gh` login (`gh auth login` on the host). Ligus never
stores or sends a token. Writes need the bearer token (or the tunnel), a box
that can write (not revoked or pending), and a roster role of at least
Contributor, and answer 403 otherwise. Cloning and `PUT /v1/chats/{id}/repo`
also need the box owner (a phone certified to the account of the Admin who
admitted the box, or the bearer on a loopback listener), else 403
`owner_required`. No path on the box is ever returned:
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
| POST | `/v1/chats/{id}/pr` `{title, body?, draft?}` | `{url, existing, branch, pushedBranch, repo, base, draft}`. Not a fork: pushes `ligus/chat/<id>` to `origin` (never forced) and runs `gh pr create` against the default branch (`draft` defaults to false). A fork: pushes the chat branch to `origin` (the fork) as `ligus/<title-slug>-<chat-id-8>` (the name is recorded in `pr-branches.toml`, so later pushes update the same branch and PR), then `gh pr create --repo <upstream> --base <upstream default> --head <forkOwner>:<branch>`, a draft unless `draft: false`. Any push whose remote resolves to the upstream repository is refused (409). A fork checkout with no `upstream` remote yet answers 409 (fetch it from the picker first). The link is posted as a system note. 412 when `gh` isn't signed in on the host; 409 when the chat branch is missing, has nothing new, or the push is refused |

- `Checkout = {name, displayName, defaultBranch?, branch?, head?, dirty, remote: {host, owner, name, fullName}|null, fork?: Fork, isWorkspaceDefault, boundChats}`.
  `displayName` is `owner/name` for a hosted remote, else the name. `dirty`
  ignores the app's own `.ligus/` (and any `.ligus/`) state. In a fork, `defaultBranch` is
  upstream's default branch.
- `Fork = {upstream: "kubeflow/pipelines", upstreamRemote: "upstream", baseBranch, upstreamPushDisabled, prTarget: "upstream", behind?, ahead?}`,
  present when the checkout has an `upstream` remote pointing at another
  repository. `behind`/`ahead` compare the local base branch with
  `upstream/<baseBranch>` as of the last fetch. The phone shows it as
  "fork of kubeflow/pipelines · PRs open upstream". New chat branches in a
  fork start from `upstream/<baseBranch>` (the box keeps the checkout's
  `ligus.chatBase` git config pointing there).
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
  `chatBranch` is `ligus/chat/<id>`. Each repo has its own, so switching a chat
  away and back resumes where it was. `ahead`/`behind` compare it with the
  default branch (`upstream/<baseBranch>` in a fork). `prBranch` is the
  branch a fork PR from this chat was pushed as. `GET /v1/chats/{id}/details` carries the same object as
  `repo`.

Approved proposals reach `ligus/chat/<id>` only when a host applies them: the
desktop's Implement, or the host a person names with **Apply on ‹host›**
(the `requestApply` phone action below). Until then the chat branch has
nothing to open a pull request for.

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
Every name is cleaned (`ligus_proto::names`: bidi, zero-width, invisible
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
| POST | `/v1/invites/join` `{code, confirmSafetyCode}` | `{workspaceId, account}`: joins a DM invite under `ligus_runtime::dm::accept_dm_invite` (addressed to this account, signed by the sender's device, key matching the shared logs; a stranger only with `confirmSafetyCode` after comparing codes; never a room the box holds). The relay must be the box's own or a public `https` one; another relay is reached only at public addresses, checked at each connect (a name re-pointed inside the network is refused), with no redirects. Behind an HTTP(S) proxy (which resolves the name itself) the box resolves that relay's name first, at join and before each sync or contact call, and refuses it when it resolves to a non-public address (a name only the proxy can resolve is left to the proxy). A workspace invite answers 409: a box serves one workspace |
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

### Add this phone: implementation spec (0.1)

The phone's P-256 Secure Enclave key becomes one of its person's devices
before it pairs or signs anything. Reference implementation:
`ligus_runtime::phone_enroll` (`PhoneCode`, `preimage`, `check_device_id`).

**1. The phone's code.** Generate the Secure Enclave key once
(`SecureEnclave.P256.Signing.PrivateKey`, access control
`.privateKeyUsage`, kept in the Keychain as `kSecAttrTokenIDSecureEnclave`)
and derive its device id from it (below). Show, on **Settings > This phone**, the code as a QR code
and as copyable text, and the fingerprint:

```
code        = "ligusphone1:" + b64u( device_id(16) ‖ pubkey(65) ‖ sig(64) ‖ label(UTF-8, ≤ 64 bytes) )
pubkey      = the key's x963Representation (65 bytes, 0x04 ‖ X ‖ Y)
sig         = ECDSA P-256 / SHA-256 over
              "ligus-phone-code-v1\0" ‖ device_id(16, RFC 4122 bytes) ‖ pubkey(65) ‖ label
              as r ‖ s (64 bytes, low S: s := n − s when s > n/2)
label       = trimmed, no control characters and none of U+00AD, U+061C, U+180E,
              U+200B–U+200F, U+202A–U+202E, U+2060–U+206F, U+FEFF, U+FFF9–U+FFFB
              (bidi overrides, isolates, marks, invisible format characters),
              ≤ 64 bytes (cut at a character boundary); a code whose label
              breaks this is refused
fingerprint = upper-case hex of SHA-256(pubkey)[0..10] (80 bits), as
              XXXX-XXXX-XXXX-XXXX-XXXX (five groups of four);
              golden: pubkey 04 01 02 … 40 (0x04 then bytes 1..=64)
              → 0ED3-A6AB-957F-F6F5-9A96
b64u        = base64url without padding
```

`device_id` is never chosen: it is the id the key derives,

```
device_id   = SHA-256("ligus-device-id-v1\0" ‖ "p256" ‖ "\0" ‖ pubkey(65))[0..16]
              with byte 6 := (b6 & 0x0f) | 0x80 (UUID version 8)
              and  byte 8 := (b8 & 0x3f) | 0x80 (RFC 9562 variant)
```

over the same 65-byte `x963Representation` as `pubkey` (never the
compressed or raw 64-byte form, and never `"ed25519"` as the algorithm).
Shown or sent as text it is the lowercase hyphenated UUID. Check the
derivation against the golden vectors in
`crates/ligus-core/tests/golden/device_id_v1.jsonl` in the app's tests. It
lives with the key: a new Secure Enclave key (a reinstall that loses the
key, Reset device key) is a new device with a new id, certified again.
A code whose id isn't its key's is refused on decode with "the phone's
device id doesn't belong to its key" (`check_device_id`), before the
desktop shows the fingerprint, so a phone that sends a stored or random id
can't be added.

**2. The desktop certifies it.** Since 0.1.1 the primary path is the reverse
QR below: the phone scans the desktop's code and its own code reaches the
desktop through the box. The fallback, **Can't scan?**: the person pastes or
types the code into **My devices > Add a phone > Can't scan?** on their
desktop (or runs `ligus phones certify
<code> --recovery-words-file PATH --fingerprint F` on a CLI identity of
theirs; without `--fingerprint` it asks at a terminal and refuses
otherwise), compares the fingerprint with the phone's screen and confirms with
their passkey. The desktop appends `DeviceCertificateAdded` with a P-256
certificate (`alg: "p256"`): the phone tier, capped by the person's role.
Refused: a code whose signature doesn't verify, a device id its key doesn't
derive, a device id already certified with another key or as a desktop or box
(impossible for a derived id, and refused by the fold too), a revoked device
id (the phone needs a new key and id).

**3. Pair.** Scan `ligus pair` as before; the enrollment's device claim
(`device: {id, sig}`, [`relay-tunnel.md`](relay-tunnel.md) §7.2 E3a) names
this device. Until the box has synced the certificate the host closes the
enrollment WebSocket with close code **4040**, reason `device_not_certified`:
show "Add this phone on your desktop first" leading with **Scan your
desktop's code** (0.1.1; the code and fingerprint under Can't scan?),
and offer to pair again once the desktop has synced (the same link works
for its 10 minutes, five attempts in all; then run `ligus pair` again). **4041** `device_revoked` and **4042** `device_claim_invalid` are
final: show them, don't retry. **4043** `provisional_busy` (0.1.1, only for
a phone that sent `enrollKey`; [`relay-tunnel.md`](relay-tunnel.md) §7.2.1)
means the box already serves 4 provisional connections: retry in a minute. A close without a reason is any other refusal
(a wrong or used pairing secret).

**4. Ask who you are.** After pairing, `GET /v1/me` (through the tunnel)
answers the workspace id and room, the config log id, the account and member
id this phone acts for, the person's role, the tier (`phone`), and the
passkey relying party (`rpId`, `origins`). Store them with the host profile;
use `account.memberId` to recognise your own messages and `rpId`/`origins`
for every `ASAuthorization` request. Nothing is typed in. Call it again after
a `403 device_not_certified` from any phone-action route.

### Add a phone by scanning the desktop's QR: implementation spec (0.1.1)

The reverse of "Add this phone": the **desktop** shows a QR code, the phone
scans it with the app's own scanner, and the phone's `ligusphone1:` code
travels back to the desktop through the workspace's `ligus serve` box. The
desktop then certifies it exactly as the paste flow does (the same
`certify_phone_code`, the same `DeviceCertificateAdded`, the same passkey).
Design: [`design/v2/phone-enroll-reverse-qr.md`](design/v2/phone-enroll-reverse-qr.md).
Reference implementation: `ligus_runtime::phone_enroll_qr` (the only code
that builds or parses a `ligusenroll*` string). Golden vectors:
`crates/ligus-core/tests/golden/phone_enroll_qr_v1.jsonl`; differential
corpus (each mutated offer, request and answer with the core's verdict,
which the Swift parser must match exactly):
`crates/ligus-core/tests/golden/differential/rust_parity_enroll_v1.jsonl`
(copy into `LigusTests/Fixtures/differential/`). Both are generated and
checked by `crates/ligus-runtime/tests/phone_enroll_qr_vectors.rs`.

**Flow.**

1. Desktop: **Settings > Profile > My devices > Add a phone > Show code**.
   It draws a 32-byte secret, sets `exp = now + 300`, and shows the offer as
   a QR code (never as text).
2. Phone: **Settings > This phone > Add to my devices** opens the scanner,
   which in this context accepts `ligusenroll1:` (and nothing else; the
   pairing scanner still takes only `ligus://`). Decode the offer and check
   its expiry (below).
3. Pick the host profile whose workspace (from `/v1/me`, or `check`) is the
   offer's `workspace_id`. None: "Pair with this workspace's box first",
   open the pairing scanner and keep the offer in memory until it expires.
   An uncertified phone reaches the box through the provisional enrollment
   (E3b, below) or, on the direct path, with its bearer token.
4. `POST /v1/phone-enroll/check`; show "Add this phone to **Ana** (*role*)'s
   devices in **acme**?" from its answer, with the warnings below, and
   **Add**.
5. Sign the request with the Secure Enclave key, build
   `ligusenrollreq1:`, `POST /v1/phone-enroll/requests`, and show the
   fingerprint large: "Your desktop shows the same fingerprint. Check it
   there, then confirm with your passkey."
6. Long-poll `GET /v1/phone-enroll/requests/{rid}?wait=25`. **Success is
   `certified: true`**: then enroll again with the same pairing window
   (tunnel) or refresh `/v1/me` (direct), and show "This phone is now one of
   your devices". Keep polling until the offer expires: a late answer may
   still carry the tripwire alarm.

**Wire formats.** Integers little-endian; `b64u` is base64url without
padding, and decoders refuse padding, `+`, `/`, whitespace anywhere and
non-canonical trailing bits (stricter than `ligusphone1:`, whose decoder
ignores whitespace in a pasted code).

```
offer     = "ligusenroll1:" + b64u( workspace_id(16) ‖ desktop_device_id(16) ‖ secret(32) ‖ exp(u64 LE, Unix s) )
            72 bytes → 96 characters after the 13-character prefix

prk       = HKDF-Extract(salt = "ligus-enroll-qr-v1", ikm = secret)        (HKDF-SHA256)
rid       = HKDF-Expand(prk, info = "rid",         16)
k_req     = HKDF-Expand(prk, info = "request-mac", 32)
k_ans     = HKDF-Expand(prk, info = "answer-mac",  32)

code_body = the phone's ligusphone1: code bytes, unchanged:
            device_id(16) ‖ pubkey(65) ‖ code_sig(64) ‖ label(UTF-8, ≤ 64)
signed    = "ligus-enroll-request-v1\0" ‖ rid(16) ‖ workspace_id(16) ‖ desktop_device_id(16)
            ‖ exp(u64 LE) ‖ code_body
phone_sig = ECDSA P-256 / SHA-256 by the Secure Enclave key over signed, r ‖ s, low S (64)
mac       = HMAC-SHA256(k_req, signed ‖ phone_sig)                                 (32)
request   = "ligusenrollreq1:" + b64u( rid ‖ workspace_id ‖ desktop_device_id ‖ exp
                                       ‖ phone_sig ‖ mac ‖ code_body )            297..361 bytes

outcome   = 1 certified | 2 declined | 3 refused (the phone's standing: a conflict or revoked)
tag       = HMAC-SHA256(k_ans, "ligus-enroll-answer-v1\0" ‖ rid(16) ‖ outcome(u8) ‖ device_id(16))
answer    = "ligusenrollres1:" + b64u( rid ‖ outcome ‖ device_id ‖ tag )            65 bytes
```

- The prefix is the version (a later format is `ligusenroll2:`); there is no
  inner version byte.
- The offer carries no name, label or relay URL: who the desktop belongs to
  comes from the box's roster (`check`), never from the QR.
- The offer isn't a URL and no `ligus://` link starts this flow: only the
  in-app scanner reads it, so a web page or another app can't start it with
  a code it made.
- The offer has no integrity of its own. A damaged offer decodes as another
  offer and is refused later, when the desktop checks the request
  (`wrong_offer` or `bad_mac`).
- `code_body` passes every check of `PhoneCode::decode` (the code signature,
  the label clamp, the derived id); the desktop certifies exactly it.
- `phone_sig` is fresh for each request: it binds the phone's consent to
  this offer, so a `ligusphone1:` code someone saw earlier can't answer it.
  The `mac` proves the sender read the QR: the box can check everything but
  the MAC, the desktop checks everything.
- `rid` travels on the wire (the box and the relay see it) and reveals
  nothing about the secret. `secret`, `k_req`, `k_ans` and the offer text
  stay in memory on the phone and the desktop only.

**Clocks.** All checks take Unix milliseconds; `exp` is seconds. The phone
accepts an offer with `now − 120 s ≤ exp ≤ now + 600 s` and refuses it
otherwise with `expired` or `too_far_ahead`; the box accepts
`now − 120 s ≤ exp ≤ now + 420 s`; the desktop accepts a request until
`now > exp`.

**Routes** (the box; full reference in
[`serve-api.md`](../docs-site/docs/operate/reference/serve-api.md#phone-enroll)):

| Method | Path | Notes |
|---|---|---|
| POST | `/v1/phone-enroll/check` `{workspaceId, desktopDeviceId, rid, exp, deviceId?}` | Writes nothing. `{workspaceId, workspaceName, person: {accountId, memberId, displayName, role, nameCollision}, desktopLabel, phone: {standing}}`. `deviceId` only off the tunnel (through it, the device the connection proved). `standing`: `new`, or `alreadyMine` (already this person's: the app still sends its request so the desktop can update the phone's remembered name, and finishes on the desktop's answer, not on the status route's `certified`, which is already true) |
| POST | `/v1/phone-enroll/requests` `{request: "ligusenrollreq1:…"}` | `202 {rid, state: "relayed"}`. Re-sending the same bytes is idempotent. The box keeps pending requests in memory until `exp`; on a `404` from the status route (the box restarted), send the request again |
| GET | `/v1/phone-enroll/requests/{rid}?wait=N` | `N ≤ 25` (off the tunnel, add `&deviceId=` when one caller has several requests). `{state: "waiting"\|"answered"\|"expired", answer?: "ligusenrollres1:…", certified: bool}`. `certified` is whether the box's roster now has this phone's certificate |

`rid`, `workspaceId` and `desktopDeviceId` in `check` are as the offer has
them (`rid` as b64u, the ids as lowercase hyphenated UUIDs); `exp` is a
number of Unix seconds.

Box refusals, `{error, message}`:

| Status | `error` | Show |
|---|---|---|
| 400 | `bad_request` | a bug on this side (bad JSON, the request didn't decode or the signature didn't verify): don't retry |
| 404 | `not_found` (status route) | the box lost the request (it restarted): submit the same request again |
| 429 | `rate_limited` | the box is busy ("try again in a minute"), or this provisional connection used its 8 requests (enroll again) |
| 403 | `device_mismatch` | the request names another device than this connection proved: don't retry |
| 403 | `device_revoked` | "This phone's device is revoked here", and offer **Reset device key** |
| 409 | `wrong_workspace` | "This code is for another workspace": pick another host profile or pair with the right box |
| 409 | `desktop_unknown` | "The desktop that showed this code isn't a device of a person here" |
| 409 | `offer_expired` | "Show a new code on your desktop and scan again" |
| 409 | `someone_elses` | "This phone belongs to someone else here": offer **Reset device key** |
| 503 | `relay_unavailable` | "This box can't reach the workspace's relay. Use Can't scan?" (also when the box isn't a certified service member yet) |
| 404 on these routes, or a `4040` close without a provisional answer | (box older than 0.1.1) | "Update ligus serve, or use Can't scan?" |

**E3b: provisional enrollment (relay tunnel).** A phone pairs only once it is
certified (E3a, [`relay-tunnel.md`](relay-tunnel.md) §7.2), so an uncertified
phone asks for a provisional channel. msg1's payload gains one optional
field, the phone's public key:

```
{"v":1, "name":…, "pk":…, "device":{"id":…, "sig":…}, "enrollKey":"<b64u 65-byte SEC1 P-256 key>"}
```

Only where E3a would close with `4040 device_not_certified`, a 0.1.1 host
checks that `enrollKey` is a valid point, that `device.id` is the id it
derives, and that `device.sig` verifies under it over the same
`ctx("device-bind", host_id, phone_static)` as E3a (otherwise `4042`), and
answers msg2 `{"v":1,"provisional":true}`: no `pid`, no `cred`. The
connection then reaches only the three `/v1/phone-enroll/*` routes (anything
else is 404), for requests about its own device id only, at most 8
requests (then `429 rate_limited`), for at most 5 minutes; a box holds at
most 4 at once and closes a fifth with **4043 `provisional_busy`** (retry in
a minute; the attempt counts). Full rules in
[`relay-tunnel.md`](relay-tunnel.md) §7.2.1. The attempt
counts against the pairing window (5 attempts) and doesn't consume the
pairing secret. Once `certified` is true, close the provisional connection
and enroll again with the same link: E3a now binds. An older host ignores
`enrollKey` and closes with 4040 as before; an older phone never sends it.
The relay sees nothing new: msg1 and msg2 are end to end.

**The answer.** `answer` is the desktop's, relayed as is; the box can't
check its tag. Check it with `PhoneOffer::check_answer(text, my_device)`
(the tag under `k_ans`, `rid`, the device id) before showing it:

| Verdict | Show |
|---|---|
| `Mine(certified)` | nothing extra; success is `certified: true` from the box |
| `Mine(declined)` | "Your desktop declined this phone." Ask for a new code |
| `Mine(refused)` | the phone's standing refused it (a conflict, or revoked): offer **Reset device key** |
| `AnotherPhone(certified)` | the tripwire alarm: "Another phone was added with this code. Tell the person at the desktop to revoke it." Don't retry |
| `AnotherPhone(declined or refused)` | someone else answered first and was turned away: "Show a new code on your desktop and scan again" |

A phone that lost the secret (it crashed) simply pairs: E3a or `/v1/me`
says whether it is certified.

**Codec refusal reasons** (`EnrollError::code`, pinned by the vectors and
the corpus): `not_an_offer`, `not_a_request`, `not_an_answer`, `malformed`,
`bad_code`, `bad_phone_signature`, `wrong_offer`, `bad_mac`,
`offer_mismatch`, `expired`, `too_far_ahead`, `bad_tag`, `offer_closed`,
`no_request`, `hold`, `fingerprint_mismatch`, `wrong_slot`, `stale_slot`,
`bad_slot_signature`, `wrong_desktop`, `device_mismatch`. The phone meets
`not_an_offer`, `malformed`, `expired` and `too_far_ahead` on a scan, and
`not_an_answer`, `malformed`, `wrong_offer` and `bad_tag` on an answer
(drop an answer that fails; keep polling).

**The phone's screens.**

- **Settings > This phone > Add to my devices** is the primary action; the
  code card moves under **Can't scan? Show this phone's code**. The code
  stays `.localOnly` (no Universal Clipboard).
- The pairing refusal screen for `device_not_certified` leads with **Scan
  your desktop's code**.
- Scanner copy: "Point at the code on your desktop (Settings > My devices >
  Add a phone)".
- **One offer at a time.** A second scan while a request is pending asks
  "Cancel the request to *Ana*'s desktop and use this code instead?"; only
  one offer is ever in memory.
- Warnings above **Add**: when `nameCollision` is true, "Two people here
  are called Ana. Make sure this is your desktop."; when this phone already
  knows its account from another host profile (`/v1/me` `account.id`) and
  the offer's `accountId` differs, "This phone acts as *Name* elsewhere.
  This code is from a different account."
- Accessibility: the scanner announces "Scanner. Point the camera at the
  code on your desktop"; a successful scan gives a haptic and a VoiceOver
  announcement; the fingerprint uses `.speechSpellsOutCharacters`; Can't
  scan? is reachable without the camera.
- `NSCameraUsageDescription`: "Ligus uses the camera to scan pairing codes
  from your box and your desktop."
- Secrets: the secret, `k_req`, `k_ans` and the offer text live in memory
  only and are wiped after the flow; never logged, never in a crash report
  or an error string.

**What the desktop and the box do** (for reference; the phone doesn't
implement these):

- The box checks everything it can (encoding, the code, `phone_sig`, the
  caller's device, the `check` rules, limits), then publishes the request
  on the relay's presence channel at `phone-enroll:<b64u rid>:<box device
  id>:<phone tag>`, one slot per request (the tag is
  `b64u(SHA-256("ligus-enroll-slot-v1\0" ‖ rid ‖ the code's device id)[..16])`;
  field `phoneEnrollRequest`, `{request, relay, at, signature}`),
  sealed under the workspace key and signed (Ed25519, context
  `ligus-enroll-relay-v1\0`) by its device key. Only a slot signed by a
  certified device of a current service member counts, so a person's own
  device can't inject a request. The box writes nothing to any log, and
  can't certify a phone.
- The desktop opens the slot and checks, in order: the slot signature,
  `rid`, `mac` (constant time), that `ws`, `D` and `exp` are the offer's,
  the expiry, `phone_sig`, the code, the phone's standing (and once more,
  itself, right before certifying). The first valid
  request locks the offer to that phone (device id and key; a new label is
  the same phone). **Add this phone** is allowed from 5000 ms after the
  request first appeared (the hold); a valid request from another phone
  before approval aborts the offer ("Two phones answered this code. Someone
  else may have seen it. Start again."). After approval the desktop keeps
  watching until `exp` and raises a revoke alert if another phone answers
  (the tripwire).
- The desktop answers at `phone-enroll-answer:<b64u rid>` (field
  `phoneEnrollAnswer`, `{answer, desktop, at, signature}`, context
  `ligus-enroll-answer-relay-v1\0`); the box relays only an answer signed by
  the offer's own desktop device. Both sides republish every 10 s while
  pending and drop slots more than 30 s ahead or 60 s old.

### Phone actions: implementation spec (0.1)

The box is its own service member and never acts as the person. What the
person does from the phone, the phone signs with its own P-256 Secure Enclave
key (the device its certificate names on the workspace config log), and the
box drafts and relays it. Reference implementation:
`ligus_core::phone_action` (`PhoneAction`, `check_phone_action_preimage`,
`parse_phone_preimage`); golden vectors in
`crates/ligus-core/tests/golden/phone_actions_v1.jsonl`.

**Device id.** The phone's workspace device id is derived from its Secure
Enclave key, never chosen: the first 16 bytes of
`SHA-256("ligus-device-id-v1\0" ‖ "p256" ‖ "\0" ‖ publicKey)`, where
`publicKey` is the 65-byte SEC1 uncompressed point (`0x04 ‖ x ‖ y`, CryptoKit
`P256.Signing.PublicKey.x963Representation`), with byte 6 set to
`(b6 & 0x0f) | 0x80` (version 8) and byte 8 to `(b8 & 0x3f) | 0x80` (the RFC
9562 variant), as a lowercase hyphenated UUID. Send it as `deviceId`
everywhere, and as the stamp's `actor.deviceId` and the envelope's
`signerDeviceId`. A certificate naming any other id never verifies, and
enrollment refuses one (`ligus_core::crypto::device_id_for`; golden vectors in
`crates/ligus-core/tests/golden/device_id_v1.jsonl`, including the key of
`p256_cryptokit.jsonl`: check them in the app's tests). A new Secure Enclave
key is a new device with a new id, enrolled again.

**Endpoints.**

1. `POST /v1/phone-actions/prepare {deviceId, action}` →
   `{prepareId, kind, eventKind, presenceRequired, preview}` plus `challenge`
   (when `presenceRequired`) or `preimage` (when not). Through the tunnel,
   `deviceId` must be the device this pairing proved at enrollment.
2. Only when `presenceRequired`: run `ASAuthorization` over `challenge` (the
   workspace RP from `preview.rpId` / `origins`, `allowCredentials` from
   the preview, user verification required), then
   `POST /v1/phone-actions/presence {prepareId, assertion: {credentialId,
   authenticatorData, clientDataJSON, signature}}` → `{preimage}`.
3. Check the preimage (below). If it fails, don't sign; show the error.
4. Unless the action is a plain chat post (`kind` `post`), ask the owner,
   for this signature only: a fresh `LAContext`
   `.deviceOwnerAuthentication` evaluation (Face ID, Touch ID or Optic ID,
   with the passcode as fallback), with
   `touchIDAuthenticationAllowableReuseDuration = 0`. It never reuses the
   app-lock unlock or an earlier signature. The prompt's `localizedReason`
   names the action ("Approve “Capped retry backoff”"), and the sheet's
   button says how it asks ("Approve with Face ID"). If it is cancelled or
   fails, nothing is signed, recorded or sent, and the sheet stays with "Not
   approved: authentication was cancelled." A device with no passcode never
   signs. Box admissions (`/v1/admissions/commit`) ask the same way. A chat
   post, from the composer or a notification's reply, signs behind the app
   lock without asking (owner decision, 2026-11-22). The exemption is an
   allow-list holding only `post`, so every other kind, and any kind added
   later, asks.
5. Sign the preimage bytes with the Secure Enclave key (ECDSA P-256,
   SHA-256), encode r‖s (64 bytes, low-S: replace s with n − s when s > n/2),
   base64url, and `POST /v1/phone-actions/commit {prepareId, signature}` →
   `{committed, eventId, kind, messageId?, requestId?}`.

Errors: 403 (not an enrolled phone of a human member, pairing bound to
another device, the fold refuses, bad signature), 409 (presence missing,
`clearLockdown` on an unlocked chat, a stale draft: "prepare again" when this
phone signed something else on that log since the draft), 403 for a
`clearLockdown` naming a lockdown no longer in force on the box (refresh
`GET /v1/guards` and prepare again), 404 (unknown,
expired after 5 minutes, or used `prepareId`), 400/422 (unknown `kind` or
field). Keep one action in flight per chat to avoid stale drafts.

**A draft that goes stale while the person confirms.** Confirming takes a
few seconds, and the log can move meanwhile. Nothing is stored and the
`prepareId` is used up: a phone-signed action's commit answers 409 (this
phone signed something else on that log), and a passkey confirmation's
`POST /v1/presence/commit` answers `409 {"error":"stale_draft","message":…,"hint":…}`
(the box's own device wrote to the same log). The phone prepares again and
asks once more on its own, at most once per tap: the passkey prompt again,
or Face ID again for the redrafted signature, with the note "Something
changed while you were confirming. Confirm again." It does that only when
the redraft shows exactly what the person confirmed (the digest and
per-draft ids aside); otherwise the sheet shows the new draft and waits for
a tap. A chat post's redraft always waits for the person's tap: the box
already holds a signature over the stale draft, so signing again on its own
would let one tap give it two signed posts it could both publish. A second 409 in a row ends on the
sheet as a calm "Not recorded" with Try again, never an error toast. Details:
`docs/DESIGN.md` 8.1c in the iOS repository.

**Actions** (`{"kind": …, camelCase fields}`, no other fields):

| kind | fields | presence |
|---|---|---|
| `post` | `chatId`, `text` (1 to 16 000 characters, not blank) | no |
| `vote` | `chatId`, `proposalId`, `diffDigest`, `approved` | workflow or human-gated proposals |
| `answer` | `chatId`, `askId`, `answer`, `optionIndex?` (send the option's text as `answer` when one is picked) | no |
| `clearLockdown` | `chatId`, `raises`: the `raiseEventId` of each signal the lockdown bar shows (from `GET /v1/guards`; 1 to 16, each once). A signal raised after the bar was drawn isn't named, so it stays in force | always |
| `requestApply` | `chatId`, `proposalId`, `diffDigest`, `hostDeviceId`, `expectedBaseCommit?`, `repoId?` | human-gated proposals |
| `withdrawApply` | `chatId`, `proposalId`, `requestId` | no |

`diffDigest` comes from `GET /v1/proposals/{id}`; recompute it from the
content you show when you can (`authz::votes::diff_digest`:
`SHA-256("ligus-proposal-digest-v1\0" ‖ for each part u32le(len) ‖ part)`
over kind, diff, changed-files JSON, workflow JSON, command JSON, proposal id,
repo id, base commit), and refuse to vote if it differs. For a workflow or
command proposal whose JSON you can't reproduce byte for byte, the box's
digest is what binds.

**Preimage checks (all MUST hold before signing).**

1. Layout: `"hive-envelope-v3\0"` ‖ workspaceId(16) ‖ sessionId(16) ‖
   sequence(i64 LE) ‖ lamport(u64 LE) ‖ eventId(16) ‖ four fields, each
   u32 LE length + bytes (scope, timestamp, stamp, payload), nothing after.
2. workspaceId = this workspace. sessionId = `chatId`, or for `clearLockdown`
   the workspace config log id (UUIDv5 of the workspace id,
   `workspace_config_session_id`).
3. Each JSON field decodes and re-encodes to the same JSON value under the
   types you implement: an unknown field anywhere is a refusal. The scope is
   the event's own (`"workspace"` for `chatLockdownCleared`, `"session"`
   otherwise).
4. Stamp: `actor.deviceId` = this phone, `sigAlg` = `"p256"`, `actor.kind` =
   `"human"`, `runtimeEffect` absent (it is only written when true), `actor.accountId` = the
   account your certificate names. Lamport bounds: `lamport` equals
   `stamp.logHead.l + 1` (the stamp's `logHead`, `{"l": lamport, "e":
   eventId}`, is the newest event of the log the box held), or `1` when
   `logHead` is absent; and `logHead.l` is below 2^64 − 1. Every fold refuses any other lamport,
   so you can trust that the event sorts no later than one past what the box
   held when it drafted, however long it is kept back. What the box held
   includes events it wrote itself: a box that writes a chain of its own
   events before drafting can still push your action past later events of
   other devices, at the cost of publishing that chain. Keep, per log, the
   lamport of the last event you signed, and refuse a preimage on that log
   whose lamport is below it, so a box can't sort your new action (a
   reject) before one you signed earlier (an approve).
5. Presence: if you ran the passkey, `stamp.presence` equals the assertion
   you produced, field for field, and the presence digest equals the
   challenge you signed: `SHA-256("ligus-presence-v1\0" ‖ workspaceId ‖ sessionId
   ‖ eventId ‖ u64le(lamport) ‖ u32le(len(payload)) ‖ payload ‖
   u32le(len(frontier)) ‖ frontier ‖ accountId)`, where `payload` is the
   payload field's exact bytes and `frontier` the JSON of
   `stamp.cfgFrontier` (empty when absent). If you didn't, `stamp.presence`
   is absent.
6. Payload, per kind:
    - `post` → `messageAppended`: `message.role` `user`, `body` = `text`,
      `author` = `actor.displayName`, `actorIdentity` = the stamp's actor,
      `kind` `message`, every other field default (not streaming, no
      related proposal or agent, no runtime, responder, reply-to, origin
      device, workflow run, tool calls, results, reactions);
    - `vote` → `proposalVoteCast`: `proposalId`, `approval.actorId` =
      `actor.id`, `approval.approved`, `approval.diffDigest` as asked;
    - `answer` → `flowRecorded` with `humanAnswered`: `askId`, `answer`,
      `optionIndex` as asked, `by` = `actor.displayName`;
    - `clearLockdown` → `chatLockdownCleared`: `chatId` as asked, and
      `raises` exactly the list you asked to clear (same ids, same order).
      Show those signals (match each id against the `GET /v1/guards`
      signals you drew the bar from) on the confirmation sheet;
    - `requestApply` → `proposalApplyRequested`: `request.proposalId`,
      `diffDigest`, `hostDeviceId`, `expectedBaseCommit`, `repoId` as asked;
    - `withdrawApply` → `proposalApplyWithdrawn`: `requestId`, `proposalId`
      as asked.

The box may choose only: eventId, sequence, the stamp's `logHead` (and so
the lamport, one past it), timestamps, the stamp's
`recordedAt`, `cfgFrontier` and `prev`, a message's `id` and `createdAt`, a
vote's `createdAt` and `role`, an answer's `atMs`, a request's `id`.
None of these grants anything the fold doesn't re-check: a post's `id` that
is already in the chat (one of your earlier messages) is refused by every
fold, so a box can't make a post rewrite an older message (refuse it
yourself when you know the id); a vote's `role` is replaced by your role at
the event's position; `cfgFrontier` older than a removal, demotion or
revocation is judged as of that cut. `hostDeviceId` is never a phone (the
fold refuses it): pick it from `GET /v1/hosts`. What a box can still do is
withhold an event you signed: it then sorts where it was drafted, before
anything written after the draft (a later vote of yours supersedes it, a
lockdown raised since stays in force), never later than the events it
held, its own included (a box that publishes a chain of its own events
before drafting moves the draft with it). It can also name an
older `logHead` (backdate within what it held), and it picks the event id
that orders events at one lamport (between two concurrent votes of yours a
rejection wins anyway). Keep one action in flight, and treat a `committed`
answer as the box's claim until the event stream shows it.

**UI.**

- The confirmation sheet shows what the phone parsed from the preimage
  (kind, chat, the text, the vote and digest prefix, the host), never the
  box's `preview`. The preview may decorate (titles) but the decision rests
  on the parsed bytes.
- Voting and Apply on ‹host› are offered from the proposal detail:
  `canVote`, `canRequestApply` and `applyRequests` come from
  `GET /v1/proposals/{id}`. The host picker lists `GET /v1/hosts` (id =
  `hostDeviceId`, label, online, `isThis`); hosts offline are shown but
  marked.
- Request states: `open` (waiting for the host; offer **Withdraw**),
  `applied` (show the short commit), `failed` (show `reason`; offer Apply
  again), `withdrawn`. Refresh on the event stream.
- Clear lockdown: offered when `GET /v1/guards` lists the chat and you are a
  human member at Contributor or above; it always asks for the passkey.
- Answer: from `GET /v1/chats/{id}/asks`, open asks only; an option sends
  its text and its index.
- Not on the phone: rebinding an agent to a host, inviting a person, DMs as
  the person.

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
starts. `ligus pair` does the same without starting a server, which is handy
while the systemd unit runs. Run it with the server's settings
(`LIGUS_SERVE_LISTEN` or `LIGUS_SERVE_PUBLIC_URL`, and `LIGUS_DATA_DIR`): it
refuses to print a link to a loopback address, and it never creates a token of
its own, so it can't hand the phone one the server doesn't accept. The link is
`ligus://pair?url=<base64url(serverURL), unpadded>&token=<token>`, where
`serverURL` is `LIGUS_SERVE_PUBLIC_URL` or `http://<listen>`. The app's URL
scheme is `ligus`: `ligus://pair?…` and `ligus://chat/<id>`. ntfy notifications
carry `Click: ligus://chat/<chatId>`.

### Remote access through a relay (optional, off by default)

`ligus serve` can also be reached without Tailscale, through a relay, over an
end-to-end encrypted and mutually authenticated tunnel (protocol:
[`relay-tunnel.md`](relay-tunnel.md), `hive-tunnel/1`). The host keeps one
outbound WebSocket to the relay and opens no inbound port. Each phone request
is dispatched into the same axum router in process, so the API and SSE
contract above is unchanged. The relay sees only ciphertext.

```sh
ligus remote enable --relay https://relay.example.com [--reg-token T]
#   or per run: ligus serve --remote-relay URL   (env LIGUS_REMOTE_RELAY;
#   LIGUS_REMOTE_RELAY=off overrides relay.json; LIGUS_REMOTE_REG_TOKEN)
ligus pair                  # v2 link: url/token plus v=2&hid&hs&relay&ps&exp
ligus pair --relay-only     # no url/token (also automatic when the direct URL is loopback)
ligus remote status         # relay, host id, live connection, phone count
ligus phones list
ligus phones revoke <pid|name> [--keep-token]
ligus remote disable | reset   # reset = new host keys: every phone re-pairs
```

- State is in `<LIGUS_DATA_DIR>/tunnel/` (directory 0700, files 0600):
  `keys.json` (Ed25519 signing key and X25519 Noise static), `phones.json`,
  `relay.json`, `status.json`, and `pending/` for the hand-off from `ligus pair`
  and `ligus phones revoke` to the running server (picked up within 2 s).
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
- `ligus phones revoke` takes effect at once: the server ends the phone's live
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
- **The box is its own member.** It joins the roster as a **Contributor
  service member** (through a box admission) and signs every event it writes, including the phone's messages,
  which appear under `LIGUS_SERVE_NAME`, and the phone's votes. Peers verify
  those events on ingest like any other device's. Approve and reject cast the
  same `ProposalVoteCast` delta the desktop's Review pane does, at this
  member's real roster role. Quorum, the role floor and the no-self-approval
  rule apply unchanged, so a proposal whose floor is above this member's role
  needs another reviewer or a promotion. Agent proposals are authorless, as
  on the desktop.
- **Tool policy.** The desktop's defaults apply:
  - Built-in file tools are offered only to runtimes that may front a local
    server (Custom, LigusDaemon, Ollama) and only when the endpoint is on this
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
  - There is no auto-apply (`LIGUS_SERVE_APPLY_ON_APPROVE` is removed): the
    box applies a proposal only to fulfil a person's apply request naming
    it, once, in the chat's worktree of its checkout.
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
- **The box is never the owner.** Since 0.1 the box is its own **service
  member** (`ActorKind::Service`): at most Contributor, its votes never count
  as a person's approval, it holds no passkey, and it admits, removes and
  revokes no one. An Admin admits it once with a passkey (desktop: workspace
  menu, **Admit a box…**; the phone; or a Kubernetes Secret the box redeems at
  start-up). `LIGUS_SERVE_OWNER` and `ligus serve enroll` were removed and stop
  with a migration error; `identity.mode` is `member` or `revoked`. What only a
  person may do from the phone is signed by the phone's own P-256 device key
  (with the person's passkey where the rules need it) and relayed by the box:
  box admissions (`POST /v1/admissions/prepare`, `/presence`, `/commit`) and
  the phone actions (`POST /v1/phone-actions/prepare`, `/presence`,
  `/commit`; spec below). Rebinding an agent, issuing a person's invite and
  DMs as the person stay on the desktop.
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
- Creating agents and editing runtimes aren't in the API. `PUT
  /v1/agents/{id}/host` exists but is an Admin's act a box (a service member)
  can't do, and the phone tier doesn't include it: rebind from the desktop.
- New agents created on the desktop default to their owner's device. Send them
  here explicitly. Once an agent is sent to a host, only that host answers its
  mentions; the desktop defers to it (and answers after 25 s only if the host
  is offline).
- A pending "send to agent to revise" is remembered in memory: if the box
  restarts before the revision arrives, the new version isn't linked to the
  old one (both stay listed).
- Approving never applies a diff. Apply on ‹host› asks one host to apply an
  approved proposal; only that host applies it, once, and reports back.

## Running and testing

```sh
ligus serve --listen 100.x.y.z:8787 --label sff --pair
ligus pair                                     # show the pairing QR code again
cargo test -p ligus-cli                        # unit + integration (test relay, fake model, fake ntfy)
LIGUS_TUNNEL_INTEROP_RELAY=http://127.0.0.1:8443 \
  cargo test -p ligus-cli interop_against_a_real_relay   # tunnel against a relay run with LANGSTROTH_RELAY_TUNNEL=open
LIGUS_SERVE_LIVE_ENDPOINT=http://<gateway>:8080/v1/chat/completions \
LIGUS_SERVE_LIVE_MODELS=gpt-oss:20b,gemma4:12b \
  cargo test -p ligus-cli serve::tests::live -- --nocapture
```

The integration tests start a real `ligus serve` against the in-process test
relay (`crates/ligus-runtime/src/test_relay.rs`), with a desktop identity that
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

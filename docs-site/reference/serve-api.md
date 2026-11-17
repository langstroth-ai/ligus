# `ligus serve` HTTP API

`ligus serve` exposes a small JSON + Server-Sent Events API under `/v1/`.
The phone app is its main client, but any client can use it. This page lists
every route in 2.0.0, checked against the router in
`crates/hive-cli/src/serve/api.rs`. The contract the phone implements is in
[`docs/ios-app.md`](https://github.com/ligus-ai/ligus/blob/main/docs/ios-app.md).

## Reaching the API

There are two ways in, and both reach the same router:

| Path | Transport | Authentication |
|---|---|---|
| **Direct** | plain HTTP to `LIGUS_SERVE_LISTEN` (default `127.0.0.1:8787`) | `Authorization: Bearer <token>`, the 64-hex-character token in `<data dir>/serve.token` (or `LIGUS_SERVE_TOKEN_FILE`). One token for every direct client; compared in constant time |
| **Relay tunnel** | the phone's Noise session through the relay (`hive-tunnel/1`) | the phone's own paired key and `hpc1` credential. Any `Authorization` header is stripped. Each request re-checks that the phone isn't revoked |

- Only `GET /v1/health` answers without a token.
- A missing or wrong token is `401 {"error":"missing or invalid bearer token"}`;
  a revoked phone gets `401` "this phone's pairing was revoked".
- There are no per-route roles at the HTTP layer. Writes run as the **box's
  identity** and are checked against its roster role, like any other device:
  a forbidden write is `403 {"error":"unauthorized: …"}`. Repo-changing routes
  need Contributor or above. A box that is revoked, or waiting for owner
  approval, can't write.
- All errors are JSON: `{"error": "…"}`.
- No TLS on the listener, no CORS headers, no general rate limit. Keep the
  listener on loopback, LAN or tailnet (the bind guard enforces this; see
  [Bind address](#bind-address)), or use the relay tunnel.

## Routes

### Health

| Method + path | Notes |
|---|---|
| `GET /v1/health` | `{"ok":true,"version":"2.0.0"}`. With a valid token it adds `workspace`, `deviceLabel` and `identity` (`mode`: `owner`, `member`, `pending` or `revoked`; `actorId`, `displayName`, `deviceFingerprint`) |

```console
$ curl -s http://127.0.0.1:8787/v1/health
{"ok":true,"version":"2.0.0"}
$ curl -s http://127.0.0.1:8787/v1/health -H "Authorization: Bearer $(cat ~/.ligus/serve.token)"
{"deviceLabel":"home-box","identity":{"actorId":"db65…","deviceFingerprint":"DFD0-62BD-5D2F","displayName":"home-box","mode":"member"},"ok":true,"version":"2.0.0","workspace":"acme"}
```

### Chats and people

| Method + path | Request | Notes |
|---|---|---|
| `GET /v1/chats` | | newest first, with unread counts |
| `POST /v1/chats` | `{title?}` | filed under the first non-archived channel |
| `GET /v1/chats/:id/messages` | `?after=<msgId>&limit=` (default 100, 1 to 1000) | also advances this box's read mark |
| `POST /v1/chats/:id/messages` | `{body}` | with `LIGUS_SERVE_DEFAULT_AGENT`, a message that mentions nobody gets `@agent` prepended |
| `POST /v1/chats/:id/typing` | `{typing}` | the phone is (or stopped) typing in this chat; see [Typing](#typing). Answers `{ok, published}` |
| `GET /v1/chats/:id/typing` | | `{chatId, typing: [{actorId, name}]}`: who else is typing there now |
| `GET /v1/chats/:id/details` | | the desktop's right-hand panel, read-only |
| `GET /v1/people` | | human roster with roles and avatars; each person's `accountId`, `safetyCode`, `keyConflict`, `lookAlike`, `devices`, `links` and `dmChatId` (see [Identity](#identity-safety-codes-invites-and-dms)) |
| `GET /v1/channels` | | channel tree in sidebar order |
| `GET /v1/agents` | | agents with runtime and host labels, and `ownerActorId` when the agent has an owner (who, besides the box's person, may grant its permission requests) |
| `PUT /v1/agents/:id/host` | `{hostId}` (`""` = the owner's device) | moves an agent to another host |
| `GET /v1/hosts` | | hosts, with `isThis` and `online` |
| `GET /v1/guards` | | dearth mode, pending members, locked chats, coordinator lease. Read-only |

A message object carries `id`, `chatId`, `author`, `role`, `body`,
`createdAt` and `streaming`; `agentId` on a named agent's reply; `actorId`
on a person's message; and on a primary `@hive` reply, `responderName` (whose
hive answered) and, when it was this box or another device of the account the
box writes as, `responderDevice`: `this` or `mine` (absent otherwise; the
phone then says "<name>'s device"). Both are left out when unset.

#### Typing {#typing}

The phone sends `POST /v1/chats/:id/typing {"typing": true}` about every
2.5 s while you edit a non-empty draft, and `{"typing": false}` when you send,
clear the draft, leave the chat or background the app. The box publishes a
presence beat for that chat to the chat's own relay room (the workspace's, or
only that DM's room for a DM), sealed under the workspace key, as the person
it writes messages as: its owner once [enrolled as the owner's
device](../ops/serve-box.md), otherwise its own identity. The beat says only
who is typing in which chat, never what.

- Same bearer or tunnel auth as every route; a chat the box can't see is 404.
- Throttled: a `true` within 3 s of the last publish only refreshes the
  expiry (`published: false`); a `false` publishes at once. More than 30
  requests in 10 s answer 429.
- Expiry: with no ping for 6 s the box publishes "stopped" itself, and
  readers drop a beat older than 8 s.
- A DM still waiting for its founder answers `409 waiting_for_founder`, like
  posting a message.

### Identity: safety codes, invites and DMs

What the box knows about people comes from the config logs it holds with a
founder pin; names are cleaned (bidi, zero-width and control characters
removed) and never an identifier: the safety code is.

Invite codes carry the workspace passphrase and epoch keys (a DM's room key),
so they move **through the tunnel or on a loopback bind only**, like
code-host tokens. On a plain-HTTP network bind these answer 403 with a message
to show: `POST /v1/invites`, `presence/prepare {action: "invite"}` (and its
`commit` when prepared elsewhere), `inspect`, `join`, and a `POST /v1/dms`
that founds a DM. Listing, opening a kept DM and the pending routes (codes
stay on the box) work on any path. The phone app sends these calls through
the tunnel when the host is paired with remote access.

| Method + path | Request | Notes |
|---|---|---|
| `GET /v1/profile` | | the box's account, its `safetyCode`, `holdsAccountKey`, `canFoundDm` and its contacts status |
| `GET /v1/people/:account/links` | | a co-member's (or the box's own) linked-identity chips with the status this box cached; never asks a forge; 404 for anyone else |
| `POST /v1/invites` | `{}` | a single-use invite link to the workspace, founder-signed when the box is the founder's. Admin; needs presence (`presence/prepare {action: "invite"}`) |
| `POST /v1/invites/inspect` | `{code}` | who it is from: `trust` `known`, `claimed`, `mismatch`, `unknown` or `none`, the founder's safety code, and whether this box can take it |
| `POST /v1/invites/join` | `{code, confirmSafetyCode}` | DM invites only (a box serves one workspace); an unsigned DM invite is refused; a stranger's needs `confirmSafetyCode`; the relay must be the box's own or public https, and behind a proxy its name must resolve to public addresses on the box (checked again before each use) |
| `GET /v1/invites/pending` | | invites in the box's contact drop box (relay v0.3); the codes stay on the box |
| `POST /v1/invites/pending/accept` | `{relay, seq, confirmSafetyCode}` | |
| `POST /v1/invites/pending/dismiss` | `{relay, seq}` | |
| `GET /v1/dms` | | the box's direct messages (one kept per person; simultaneous opens archived) |
| `POST /v1/dms` | `{accountId}` | opens or founds a DM with a co-member; the signed invite is dropped into their contact box (`delivered`) or returned to share. Not on a box acting for its owner |
| `GET /v1/chats/:id/founder` | | `{waiting, founderName?}`: sends wait for the founder's signed founding (a send answers 409 `waiting_for_founder`) |

A DM's chat is listed by `GET /v1/chats` with a `dm` object and takes
messages through `/v1/chats/:id/messages`; agents never run there and its
messages never push.

### Passkey presence

On a box enrolled as its owner's device, the owner's passkey confirms each
vote, reject, lockdown clear and agent rebind (see
[Passkey presence](../security/index.md#presence)).

| Method + path | Request | Notes |
|---|---|---|
| `POST /v1/presence/prepare` | `{action: "approve"\|"reject"\|"lockdown_clear"\|"agent_host"\|"invite", target, hostId?}` | drafts the event as the plain endpoint would and writes nothing: `{prepareId, challenge, preview: {kind, summary, digestShort, rpId, allowCredentials, chatTitle?, proposalTitle?, diffDigest?}}`. `challenge` is the base64url event digest the passkey signs (RP `ligus.ai`, user verification required). `digestShort` is the first 16 hex characters (64 bits) of that digest; show it so the person can compare it with the one the passkey page computes from the challenge it signs. `prepareId` is single use and lasts 5 minutes. 409 when there is nothing to confirm |
| `POST /v1/presence/commit` | `{prepareId, assertion: {credentialId, authenticatorData, clientDataJSON, signature}}` (base64url) | appends the event with the assertion inside its signed stamp: `{committed, kind, eventId, invite?}` (`invite`, for `action: "invite"`: the code to share). 403 when the assertion doesn't verify or the passkey isn't the owner's; 404 for an unknown, expired or used `prepareId` |

### Proposals

| Method + path | Request | Notes |
|---|---|---|
| `GET /v1/proposals` | `?status=open\|all` (default `open`) | newest first |
| `GET /v1/proposals/:id` | | adds `myRole`, `myVote`, `canVote`, `isMine`. Every proposal also carries the review badges `authorKind`, `authorName`, `configImpact`, `humanGate`, `presenceRequired`, `argv` (a structured command) and `implementPlan` |
| `POST /v1/proposals/:id/approve` | | votes approve at the box's role; applies the change only with `LIGUS_SERVE_APPLY_ON_APPROVE`. On a box enrolled as the owner's device: `403 {"error":"presence_required","hint":…}` (as for `reject`, `lockdown/clear` and `agents/:id/host`); use the presence endpoints below |
| `POST /v1/proposals/:id/reject` | `{reason?}` | the reason becomes a thread comment |
| `GET /v1/proposals/:id/comments` | | refinement thread |
| `POST /v1/proposals/:id/comments` | `{text, requestChanges?}` | a Viewer gets 403 |
| `POST /v1/proposals/:id/request-changes` | `{note?}` | sends the proposal back to its agent (or the default agent). 409 if already revised or applied |
| `GET /v1/proposals/:id/implement-plan` | | exactly what Implement does: `{plan: {kind, argv?, cwd?, instruction?, targetAgentId?, targetName, legacyCommand}}` |
| `GET /v1/chats/:id/asks` | | agents' questions in the chat, newest first: `{asks: [{id, agentName, kind, question, options, open, answer?, answeredBy?, expiresAtMs}]}`. `kind` is `question` (an agent's `ligus_ask_human`) or `permission` (an ACP permission escalation, which only the host's person or the agent's owner can grant) |
| `POST /v1/asks/:id/answer` | `{answer?, optionIndex?}` | answers an agent's question as the box's person (the first answer wins; it authorizes nothing, so it needs no presence). 400 if already answered or closed |
| `GET /v1/proposals/:id/file` | `?path=<relative>&side=old\|new` | one whole file before or after the diff, rebuilt in a scratch dir. 400 bad path, 404, 409 base moved, 413 over 512 KiB, 415 not UTF-8 |

### Repos on the host

| Method + path | Request | Notes |
|---|---|---|
| `GET /v1/repos` | `?q=&owner=&page=&perPage=&forge=` | local checkouts and running clone jobs, plus: without `forge`, `github` (the GitHub connection's repos, as before) and `forges` (every connection); with `forge=<id>`, that connection's repos under `forge` |
| `GET /v1/repos/fork` | `?repo=owner/name&forge=` | the signed-in account's fork, if any. 412 if the connection isn't signed in |
| `POST /v1/repos/clone` | `{repo, viaFork?, forge?}` | Contributor. Starts a clone (or fetch) job. A URL's host picks the connection; `owner/name` uses `forge` (default `github.com`). 400 for a host with no connection; 429 with 2 jobs running; 409 if the target dir holds another repo |
| `GET /v1/repos/clone/:job` | | job status (last 20 finished kept) |
| `GET /v1/chats/:id/repo` | | the chat's checkout, branch, ahead/behind, PR branch |
| `PUT /v1/chats/:id/repo` | `{repo?}` | Contributor. Binds a checkout under the repos dir; `null`/`""` = workspace default |
| `POST /v1/chats/:id/repo/pull` | | Contributor. Fast-forward-only pull; 409 if dirty or diverged |
| `POST /v1/chats/:id/pr` | `{title, body?, draft?}` | Contributor. Pushes `hive/chat/<id>` (never forced, never to upstream) and opens a pull or merge request on the checkout's code host. The answer adds `kind` (`pr` or `mr`) and `forge` |

### Code hosts

The box's code-host connections ([Code hosts](../features/code-hosts.md)).
No answer ever carries a token.

| Method + path | Request | Notes |
|---|---|---|
| `GET /v1/forges` | | `{forges: [{id, kind, flavor, displayName, baseUrl, authSource, user, available, reason, hasRead, hasWrite, deviceFlow, openLabel, changeRequestKind, holder}], tokenEntry}`. `tokenEntry` says whether this channel may carry a token |
| `POST /v1/forges` | `{kind, baseUrl, auth?, clientId?, internal?}` | Contributor and presence. `kind` is `github`, `gitlab`, `forgejo`, `gitea` or `codeberg`; `baseUrl` must be `https`. A non-public address needs `internal: true` (kept on this box only). 409 if already connected |
| `DELETE /v1/forges/:id` | | Contributor and presence. Removes the connection and its tokens |
| `POST /v1/forges/:id/test` | | Contributor and presence. Signs in; `{ok, user?, userId?}` or `{ok: false, error}` with a fixed message, never the host's reply |
| `PUT /v1/forges/:id/token` | `{purpose: read\|write, token}` | Contributor and presence, **through the tunnel or on a loopback bind only** (403 otherwise). Stored in the box's secret store |
| `DELETE /v1/forges/:id/token` | `?purpose=read\|write` | Contributor and presence |
| `POST /v1/forges/:id/auth/device` | | Contributor and presence, tunnel or loopback only. `{userCode, verificationUri, expiresIn}`; the box polls |
| `GET /v1/forges/:id/auth/device` | | Tunnel or loopback only. `{state: pending\|done\|denied\|expired\|error, userCode, verificationUri, expiresIn}` |

Connection ids are `<kind>:<host>`, for example `github:github.com`,
`gitlab:gitlab.example.com` or `gitea:codeberg.org`; `github.com` alone also
names the GitHub connection.

What the phone may touch on disk is limited; see
[Host path policy](../features/phone-app.md#host-path-policy).

### Host extras

| Method + path | Notes |
|---|---|
| `GET /v1/dashboard` | the host dashboard; see [Host dashboard](../features/dashboard.md) |
| `GET /v1/lab/status` | **deprecated**; answers with `Deprecation: true` and a `Link` to `/v1/dashboard` |
| `GET /v1/appearance` | `{palette, mode, source}` for *Match host* |

### Live events

`GET /v1/events` is an SSE stream. It opens with a `: connected` comment,
replays the backlog after the `Last-Event-ID` header (a ring of 1024
events), then streams live, with a keep-alive every 15 s. Event names:
`message.created`, `message.delta`, `message.done`, `proposal.created`,
`proposal.updated`, `chat.updated`, `turn.status`, `ask.pending` (an
agent's question or permission request, with its `kind`, also pushed to
phones), and `typing` (`{chatId, typing: [{actorId, name}]}`, the full
list of other people typing in that chat, an empty list when they stop; you
and the person the box writes as are never in it; names are cleaned of
control and bidi characters). `typing` is live only: it isn't kept for
`Last-Event-ID` replay and never becomes a push. While a stream is open the
box reads its rooms' presence every 2 s. Through the tunnel, an
open stream marks the app as in the foreground, so the box doesn't push to
that phone.

### Tunnel-only

These answer 404 on the direct path.

| Method + path | Request | Notes |
|---|---|---|
| `POST /v1/tunnel/credential` | | renews this phone's `hpc1` credential (90 days) → `{credential, exp}` |
| `PUT /v1/tunnel/push` | `{enabled, previews?}` | this phone's push preferences |

## Bind address

`--listen` or `LIGUS_SERVE_LISTEN`, default `127.0.0.1:8787`. `ligus serve`
refuses to start on any address that isn't loopback, private (RFC 1918),
link-local, CGNAT `100.64.0.0/10` (Tailscale) or IPv6 ULA `fc00::/7`, and
always refuses `0.0.0.0`/`::`, because the API is plain HTTP behind one
bearer token. `LIGUS_SERVE_ALLOW_ANY_BIND=1` lifts the guard; use it only
where something else decides who reaches the port (a Kubernetes pod with a
Service and NetworkPolicies).

## Limits and timings

| What | Value |
|---|---|
| Relay sync | every 3 s while the relay's push stream is down, every 20 s as a safety poll while it's up |
| Turn on shutdown | up to 15 s to finish; otherwise marked interrupted |
| Clone jobs | 2 at once; `LIGUS_REPOS_MAX_CLONE_MB` (2048) and `LIGUS_REPOS_CLONE_TIMEOUT_SECS` (900) |
| Tunnel | 8 sessions (or the relay's `maxSessions`), 32 streams per session, 16 KiB frames; handshake 10 s, idle 90 s, session 24 h |
| Push | see [Push notifications](../features/phone-app.md#push-notifications) |
| Token rotation | the server re-reads `serve.token` every 2 s |
| Typing | republish at most every 3 s, expire after 6 s quiet, 30 requests per 10 s, others polled every 2 s while a stream is open |

There is no separate readiness endpoint; use `/v1/health`. Relay sync state
shows in the dashboard's built-in section and in `ligus remote status`.

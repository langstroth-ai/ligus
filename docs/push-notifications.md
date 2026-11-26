# Push notifications (iPhone)

`ligus serve` can wake the Ligus iPhone app with a notification when an agent
finishes a reply, someone mentions you, a change is ready for review, or a
workflow waits for your approval. The notification content is end-to-end
encrypted: the relay and Apple see only ciphertext. The wire format is
[`relay-tunnel.md` §14](relay-tunnel.md#14-end-to-end-encrypted-push); this
page covers what the host sends, when, and how to turn it on for your own
setup.

Push needs remote access through a relay ([`ios-app.md`](ios-app.md#remote-access-through-a-relay-optional-off-by-default)).
Without it, `ligus serve` can still notify through a self-hosted ntfy server
(`--ntfy-url` / `--ntfy-topic`), which sends no message content at all:
only generic text ("A reply is ready") unless the operator sets
`LIGUS_NTFY_TITLES=1`. The URL must be `https://` unless
`LIGUS_NTFY_ALLOW_HTTP=1`.

## How it works

```
ligus serve ──(control WebSocket)──▶ relay ──(HTTP/2, APNs key)──▶ APNs ──▶ iPhone
  encrypts per phone                 forwards ct                       Notification Service
  with that phone's push key         + the phone's APNs token          Extension decrypts and
                                                                       shows title/body
```

1. When a phone pairs, both ends derive a 32-byte **push key** from the
   enrollment handshake. The host keeps it in `tunnel/phones.json`; the app
   keeps it in a Keychain access group shared only with its Notification
   Service Extension.
2. On an event, `ligus serve` builds the plaintext below and seals it for each
   phone that wants pushes: `ChaCha20-Poly1305(push_key, nonce, plaintext,
   ctx("push", host_id, pid))`. It sends `{"t":"push","pid","ct","prio","collapse"}`
   on its control connection.
3. The relay looks up the APNs token the phone registered when it connected,
   and sends APNs a generic alert ("Ligus" / "New activity on your host") with
   `mutable-content: 1` and the ciphertext.
4. The app's Notification Service Extension decrypts it with the push key,
   checks `ts` (within 1 h) and `seq` (strictly increasing, so a replay shows
   only the generic text), and replaces the title and body. Tapping it opens
   `ligus://chat/<chatId>`. If anything fails, the generic text stays.

## Events and payload

Plaintext (JSON) of every push:

| Field | Type | Present | Meaning |
|---|---|---|---|
| `v` | `1` | always | format version |
| `seq` | u64 | always | per (host, phone), strictly increasing, survives restarts |
| `ts` | seconds | always | when the host sent it |
| `kind` | string | always | see below |
| `title` | ≤ 64 bytes | always | names the sender and the event, **never** message text |
| `body` | ≤ 512 bytes | always | the preview, or `""` with previews off |
| `chatId` | UUID | chat events | deep link `ligus://chat/<chatId>`; also the collapse key (HMACed) |
| `sender` | ≤ 64 bytes | `message.done`, `mention` | the agent or member name alone |
| `proposalId` | UUID | `proposal.created`, `workflow.approval`, `review.recruit` | the proposal to review |
| `workflowRunId` | UUID | `workflow.approval` | the run waiting on the gate |
| `workflowId` | UUID | `workflow.approval` | the run's workflow definition |
| `workspaceId` | UUID | `approval.request` | the workspace the request asks to join |
| `rid` | 32 hex | `approval.request` | the approval request (`GET /v1/approvals` lists it) |
| `label` | ≤ 64 bytes | `approval.request` | the label the requesting box gave itself (a display hint, not verified) |
| `requestKind` | `box` | `approval.request` | what asks: always `box` today (see below) |
| `preview` | `true` | when `body` is a content preview | |

Unknown fields and kinds must be ignored by the app (an unknown kind shows
`title`/`body` and links `chatId` if present).

| `kind` | When | `title` | `body` (previews on) |
|---|---|---|---|
| `message.done` | an agent reply to **you** finished: you are its requester, the person whose message started the turn (through hand-offs; a workflow stage's is whoever started the run). Other members get nothing for it unless it mentions them (then one `mention`). No requester found → nobody. Not a reply that mentions you (that is the `mention`) | `coder replied`, or `coder hit an error` for a failed turn | first 120 characters of the reply |
| `mention` | a message from an agent or another member `@mentions` you | `Ana mentioned you` | first 120 characters of the message |
| `ask.pending` | an agent asks a question and you are in its audience (named, or at or above its room role); never for one a member posted under an agent's name. At most 10 an hour, then one `Several agents are asking` until you open the questions | `@coder asks you · <chat>` (or `asks Admins`, `asks Owners`, `asks the room`) | the question |
| `proposal.created` | a proposal is waiting for review (not a workflow gate's) | `Change ready for review` | the proposal title, cut the same way |
| `workflow.approval` | a workflow run's approval gate is waiting (again on each rerouted attempt) | `<workflow name> needs approval` | the gate's stage name |
| `review.recruit` | a backed-up review queue asked you to help (a `flowRecorded` recruit with action `approvers` naming you) | `The review queue needs you` | the title of the proposal that has waited longest for you (one you haven't voted on and didn't write); `proposalId` deep-links to it |
| `approval.request` | a box asks to join the workspace through its approval mailbox (code-match approval, `docs/design/code-match-approval.md` 7.3) and nobody is checking it yet | `<label> wants to join <workspace>` | `Open Ligus to compare the code the box shows.` (always shown; no preview) |
| `phone.paired` | a new phone was paired with this host | `New phone paired` | the phone's name (always shown) |
| `test` | reserved for a test push | | |

Rules:

- **One notification per event.** A reply that both finishes and mentions you
  is one `mention`. A workflow gate is announced once as `workflow.approval`,
  not also as `proposal.created` (its backing proposal is recognised by the
  run's `proposalId`, or as an authorless decision when the run hasn't synced
  in yet).
- **"You"** is worked out for each phone: the person who holds its
  workspace device (the member its requests act for), and any member that
  shares their account, such as their desktop. A box that joined as its own
  member is never "you": a question for the phone's person reaches their
  phone, and another person's phone on the same box gets nothing for it. A
  phone whose person can't be told (a pairing without a device, a contested
  device) uses this host's identity. ntfy follows this host's identity.
  A mention is `@<your name>` (or `@<name>#<n>`), `@all`, a role group you're
  in (`@owners`, `@admins`, …), or `@you` in an agent's reply whose requester
  (the person whose message started the turn) is you. A person's `@you` is
  nothing; an agent's `@you` with no requester reaches nobody; an agent's
  `@all` within 10 minutes of another agent `@all` in the chat reaches only
  its requester. Your own messages never notify. Message JSON for an agent
  reply with `@you` carries `requester {name, actorId, isYou}` so the phone
  renders `@you` as `@<name>`.
- **Join requests.** `ligus serve` polls the workspace's approval mailboxes
  (the current key epoch and the two before it) as a member and sends one
  `approval.request` per new box request, the first time it sees it, unless
  it is already claimed or ended. There is no `chatId`: these pushes
  collapse per workspace, and the host coalesces them per request. After
  **3 in 10 minutes** per workspace it stops pushing them, and the phone's
  Inbox shows one row, "Several join requests", so a flood can't wear the
  person down. A computer's or a phone's request ("a device of mine") never
  pushes: `GET /v1/approvals` lists it so the phone can show it read-only
  ("Approve on your desktop"), without a code, because the `open` and the
  reveal are sealed to the requester and the approver, so neither the box nor
  the phone can compute that code. A box restart forgets which requests it
  pushed, and may push a still-live one once more. The phone is the approver
  for a box: the box only carries its sealed messages
  (`docs/design/code-match-wire.md` section 5).
- **Only news.** Replies, proposals and gates created more than 10 minutes
  before `ligus serve` started (history that synced in late) don't push, and a
  restart never replays.

## Per-phone settings

Each paired phone has three preferences, set by the app through the
tunnel-only route `PUT /v1/tunnel/push {"enabled": bool, "previews"?: bool,
"kinds"?: [string]}`:

- `enabled` (default off until the app asks for permission): pushes go only
  to active (not revoked) phones with `enabled` on.
- `previews` (default on): with previews off, `body` is empty and `preview`
  is absent, so the notification says only who did what ("coder replied").
  The phone's settings screen has the toggle. If an encrypted push with its
  preview would exceed 2 048 bytes, the host sends it without the preview.
- `kinds` (default: every kind, for an app that never sent it): the push
  kinds the phone's notification switches allow. Of `proposal.created`,
  `workflow.approval`, `review.recruit`, `ask.pending`, `approval.request`,
  `mention` and `message.done`, the host sends only the ones listed; any
  other kind (`phone.paired`, `test`) isn't one of the switches and always
  goes. A body without `kinds` keeps the stored list.

Forget host on the phone turns `enabled` off on that box before it deletes
the pairing (best effort: an offline box keeps it until the pairing is
revoked with `ligus phones revoke`).

## Host-side limits

On top of the relay's limits (§11), `ligus serve` drops a push to a phone when:

| Rule | Limit |
|---|---|
| Foreground | the phone has an open `/v1/events` stream through the tunnel and sent a frame (a request, PING or WINDOW) within the last 40 s. The app keeps that stream open only while it's in the foreground, PINGs after 25 s without traffic, and closes the session (RESET, GOAWAY) when it goes to the background. A phone iOS suspended before that close left goes quiet, so its stale stream stops counting after 40 s |
| Coalescing | one push per (phone, kind, chat) per 10 s (an `approval.request`: per request): the first of a burst goes, the rest are dropped |
| Join requests | 3 `approval.request` per workspace per 10 minutes, one per request |
| Per phone | 20 per minute, 500 per day |
| Host-wide | 1 per second sustained, burst of 5 (the relay's "pushes ≤ 1/s") |

A dropped push doesn't count against any limit. Every push decision is
printed as a `push:` line (kind, phone name and the start of its pid; never
content): sent, not sent and why (app open, notifications off on that phone,
coalesced, rate-limited, holding after a refusal, push unavailable), and the
relay's answer. `ligus remote push-test` sends a `test` push with
`"diag": true`, past the host's own gates, and prints each hop including the
relay's `detail` (APNs env, topic, status, reason).

The relay acks each push. On `ok:false` the host:

- `push_unavailable`: turns push off until the next relay connection (whose
  `welcome` reports `push`) and says so once on stderr;
- `no_push_token`: holds pushes to that phone for 10 minutes, or until the app
  connects again (it registers its token when it connects);
- `revoked`: holds pushes to that phone for 1 hour;
- `rate_limited`: holds all pushes for 60 s;
- `push_failed`: nothing; that one push is lost.

Nothing is retried: a notification is a hint, and the app catches up through
`/v1/events` when it opens.

## Turning push on for yourself

APNs only accepts a push for an app when it is signed with an APNs key from
the Apple developer team that publishes that app. So push works when you run
the relay with your own key **and** build and sign the iOS app yourself under
the same team.

### 1. Apple Developer portal

1. In **Certificates, Identifiers & Profiles → Keys**, create a key with
   **Apple Push Notifications service (APNs)** enabled. Download the
   `AuthKey_<KEYID>.p8` file. Apple lets you download it only once: keep it
   secret, like a password. Note the **Key ID** and your **Team ID** (top
   right of the portal, or Membership details).
2. On the app's identifier(s) (your development bundle id, and the release
   bundle id if you ship one), enable the **Push Notifications** capability
   (and register the extension's id, `<bundle id>.notifications`). With
   automatic signing in Xcode, signed in with an Apple ID of that team
   (Xcode → Settings → Accounts), this happens for development builds when
   you build with push turned on (step 4). Without an account, Xcode falls
   back to a wildcard profile, which can't carry Push, and the push build
   fails to sign.

### 2. Run the relay with the key

Use the open relay ([langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay),
`cmd/langstroth-relay`) with tunnels on and the APNs settings:

```sh
LANGSTROTH_RELAY_TUNNEL=open                      # or allowlist + LANGSTROTH_RELAY_TUNNEL_HOSTS=<host id>
LANGSTROTH_RELAY_APNS_KEY_FILE=/path/to/AuthKey_KEYID.p8   # or LANGSTROTH_RELAY_APNS_KEY=<inline PEM>
LANGSTROTH_RELAY_APNS_KEY_ID=<Key ID>
LANGSTROTH_RELAY_APNS_TEAM_ID=<Team ID>
LANGSTROTH_RELAY_APNS_TOPIC=<the app's bundle id>
LANGSTROTH_RELAY_APNS_ENV=auto                    # optional: auto | sandbox | production
```

`auto` uses the environment each phone reports: `development` for Xcode
(Debug) builds, `production` for TestFlight and App Store builds. Keep the
`.p8` file readable only by the relay's user, and don't commit it.

Check it: `GET /v1/tunnel/info` on the relay reports `"push": true`.

### 3. Point the host at the relay and pair

```sh
ligus remote enable --relay https://<your relay>
ligus serve                  # or restart the service
ligus pair                   # scan with the phone
ligus remote status          # "push available" once connected to a relay with a key
```

### 4. Build the app with push on

In the iOS app's repository (`ligus-ios`), set in `Config/Local.xcconfig`:

```
LIGUS_PUSH_NOTIFICATIONS = YES
```

plus your own team and bundle id overrides (see `Config/Local.xcconfig.example`).
Build and install, open the app, and allow notifications when asked. The app
then registers its APNs token with the relay and turns `enabled` on for this
host. **Settings → Notifications** in the app toggles previews.

### 5. Troubleshooting

- `GET /v1/tunnel/info` says `"push": false`: the relay has no APNs key
  configured, or failed to load it (check its log). The host then turns push
  off and falls back to ntfy if configured.
- **`DeviceTokenNotForTopic`** in the relay log: the topic used isn't the
  bundle id of the build on the phone. Current apps report their bundle id
  (`apns.topic`) and a v0.3 relay pushes with it when it is allow-listed
  (`LANGSTROTH_RELAY_APNS_TOPIC` / `LANGSTROTH_RELAY_APNS_TOPICS`); `topic not allowed`
  means add it there. For an older app (no topic) the relay keeps the token;
  fix the default topic.
- **`BadDeviceToken`**: sandbox vs production mismatch, e.g. an Xcode build
  sent to the production endpoint. Use `LANGSTROTH_RELAY_APNS_ENV=auto`, or match it
  to the build.
- Nothing arrives while the app is open: expected, the foreground stream
  suppresses pushes. Background the app and try again.
- Nothing arrives at all: check that the phone is paired and active
  (`ligus phones list`), that notifications are allowed for the app in iOS
  Settings, and run `ligus remote push-test`; the `push:` lines in the serve
  log show every decision and relay refusal.

## Limitation: other people's setups

Because APNs ties pushes to the publisher's key, a self-hosted relay can push
only to builds of the app signed by the same Apple team as the relay's key.
Someone running their own relay can't push to the App Store build of the app;
they need to build and sign the app themselves with their own team, bundle id
and key (steps 1–4).

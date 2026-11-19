# Ligus on your phone

The Ligus iPhone app is a thin client for one person's workspace. The phone
doesn't run Ligus and holds no workspace keys. It talks to **`ligus serve`**, a
Ligus device you run on an always-on box, over a small JSON + Server-Sent
Events API (`/v1/…`). The box keeps the end-to-end encryption keys, syncs with
your relay and runs the agents you send to it; the phone reads and writes
through it.

> The app's source lives in its own repository. The `ligus serve` side and the
> API contract the app uses are in this repo
> ([`docs/ios-app.md`](https://github.com/langstroth-ai/ligus/blob/main/docs/ios-app.md)),
> so any client can implement them.

To set up the box and pair a phone, see
[Remote access for your phone](../../operate/networking/remote-access.md). The box
is its own service member, admitted once with your passkey
([Admitting a box](../../operate/ops/admitting-a-box.md)). The box never acts
as you: what only you may do (post as you, approve or reject, answer an
agent's question, clear a lockdown, Apply on ‹host›) the phone signs with
**its own device key**, and your passkey where the rules need it; the box
drafts the event and relays it ([Phone-signed actions](#phone-signed-actions)).
The box's own votes never count as your approval
([Running ligus serve](../../operate/ops/serve-box.md)). For the whole
setup end to end, see [Personal fleet](../../operate/deploy/personal-fleet.md).

## Add this phone {#add-this-phone}

Before the phone can pair with your box or sign anything, your desktop
certifies the phone's own device key (a P-256 key in the Secure Enclave) as
one of **your** devices:

1. In the app, open **Settings > This phone**. It shows the phone's code
   (`ligusphone1:…`, as a QR code and as text to copy) and its fingerprint
   (`XXXX-XXXX-XXXX-XXXX-XXXX`, five groups of four).
2. On your desktop, open **Settings > Profile > My devices > Add this phone**
   and paste the code (copy it on the phone and paste on the Mac with the
   shared clipboard, or scan the QR code with the phone's camera app and
   paste what it reads).
3. Check that the fingerprint the desktop shows is the one on the phone,
   tick the box, and choose **Add this phone**. Your passkey confirms it;
   the passkey sheet names the same fingerprint. The phone is added to the
   workspace you checked the code in: if you switch workspaces before
   confirming, the desktop refuses and asks you to check the code again.

A phone code can't take over a device. A phone's device id is derived from
its key, so a code naming any other id (one of the workspace's desktops or
boxes, another phone, or an id the phone chose) is refused by the desktop
before it shows the fingerprint, and a certificate for it by every member's
fold.

The phone is certified at the **phone tier**: it can post, react, vote,
answer an agent's question, clear a lockdown and ask a host to apply, as you
and capped by your role; it never changes members, devices or keys. Without
a desktop, `ligus phones certify` does the same on a CLI identity of yours
with your recovery key ([CLI reference](../reference/cli.md)).

A phone that tries to pair before the box has synced its certificate is
refused with `device_not_certified` (the app says to add the phone first);
pair again once the desktop has synced. After pairing, the app asks the box
`GET /v1/me` and fills in the workspace, your account, your role and the
passkey relying party by itself.

## Pairing: direct or through the relay

The app connects to a box in one of two ways, and a host profile can use
both:

| Path | How | What the phone stores |
|---|---|---|
| **Direct** | plain HTTP to the box's address (`http://<host>:8787`) on your LAN or tailnet, with the box's bearer token | the URL and `serve.token` |
| **Relay** | a Noise session through your relay's tunnel; the relay forwards ciphertext only | the box's host id and public key, the relay URL, and the phone's own key and credential (90 days, renewed daily while used) |

Pair by scanning the QR code `ligus pair` prints (or opening its
`hive://pair?…` link; `ligus://` works too). A **v2** link carries the relay
details as well as, when there is a usable direct address, the URL and token.
A box listening only on loopback (`127.0.0.1`) with remote access off prints
no link, since the phone can't reach it; see
[Remote access](../../operate/networking/remote-access.md).
You can also add a host by typing its address; the app assumes
`http://…:8787`.

Each host profile has a **path** setting:

- **Auto (direct, then relay)**: use the direct address when it answers
  quickly, otherwise the relay;
- **Direct only**;
- **Relay only**, even when the direct address would answer.

The app can hold several host profiles, each stored as its own Keychain
item. Credentials live in the iOS Keychain; the phone never holds a
workspace key. Its own device key lives in the Secure Enclave and never
leaves it ([Add this phone](#add-this-phone)).

## Host path policy

What the phone can reach on the box's disk is deliberately narrow:

- **Checkouts** come only from the box's repos dir (`LIGUS_REPOS_DIR`,
  default `<data dir>/repos`). A chat can be bound only to a plain directory
  name directly inside it that is already a git repository. Bindings to
  other paths are made on the box with `ligus bind-repo`; the phone sees only
  their last path component.
- **Clones** accept only `owner/name` or `https://github.com/owner/name`;
  other hosts, schemes, credentials in URLs and query strings are refused.
  They land in `<repos dir>/<owner>__<name>`, limited by
  `LIGUS_REPOS_MAX_CLONE_MB` (2048) and `LIGUS_REPOS_CLONE_TIMEOUT_SECS`
  (900), two at a time.
- **Proposal files** (`GET /v1/proposals/{id}/file`) take a plain relative
  path: no `..`, no absolute paths, nothing under `.git`. The file is rebuilt
  in a scratch directory, never in the checkout.
- **GitHub** actions use the box's own `gh` login. Ligus never force-pushes,
  never pushes to an upstream it doesn't own and never creates forks.
- Writes that change repos need the box's role to be Contributor or above.

## App lock (Face ID)

**Settings → Security → Require Face ID / Touch ID** locks the app, with your
passcode as fallback. It is off by default and offered once after the first
pairing. Choose when it locks again: immediately, after 1 minute or after 5
minutes in the background. A privacy cover hides content in the app
switcher. Networking keeps running while the app is locked. The lock protects
the app on an unlocked phone; it isn't a substitute for
[revoking a lost phone](../../operate/security/runbooks.md#lost-phone).

## Demo mode

**Try the demo** on the pairing screen opens a sample workspace that runs
entirely on the phone, with no box or relay. Replies come from Apple's
on-device model where available, otherwise from scripted text. Nothing is
sent anywhere, and there's no push in the demo.

## Chats

- The chat list shows each chat's title, a preview of the newest message, an
  unread count and up to five recent participants. Chats are filed under their
  channels, in the desktop's sidebar order.
- Start a chat or send a message from the phone. `@mentions` route exactly as
  on the desktop: `@agent` wakes that agent, `@hive` the chat's primary
  runtime. See [Multi-agent collaboration](multi-agent.md).
- Replies stream live. A reply produced on the `ligus serve` box streams token
  by token; one produced on another device (your desktop, a worker) appears
  once it syncs, a few seconds later.
- **Typing.** While you write a message, the people in the chat see "<your
  name> is typing" on their desktops and phones; you see who else is typing,
  set apart from the agent's own "thinking" row. It shows only that someone
  is typing, never what, is encrypted for the workspace like everything else,
  and never sends a notification. A DM's typing goes only to that DM. Turn it
  off in **Settings → Share typing status** (on by default; the desktop has
  the same switch under **Settings → Profile → Privacy**). Nothing is sent
  while a chat waits for its founder.
- **Which device answered.** A reply from the chat's primary `@hive` says
  where it ran: "This device" (the box), "Your other device", or "<name>'s
  device".
- Unread counts and read marks belong to the box, not to each device:
  opening a chat on the phone marks it read there, and your desktop keeps its
  own read state.

## People, channels and chat details

- **People** lists the workspace's human members with their role, title and
  avatar. The box's own identity is marked as you.
- **Channels** shows the channel tree, including each channel's drop-in chat.
- **Chat details** is a read-only view of the desktop's right-hand panel: who
  is in the chat, which agents it uses and where each one runs, who answers an
  untagged message (and whether that device is online right now), a rough
  context-window estimate, recent workflow runs, and the chat's skills and
  vaults.

!!! note "Who answers `@hive`"
    Only a desktop runs a chat's primary runtime (`@hive`). Chat details says
    whether your desktop is available. If it's closed, the turn waits and is
    answered when the desktop opens. For chats the phone creates to be
    answered by your desktop, enroll the box as
    [your own device](../../operate/ops/serve-box.md).

You can also move an existing agent to a different host from the phone.
Creating agents and editing runtimes stay on the desktop.

## Safety codes, invites and direct messages

The **People** button in the chat list (and **Settings → People and safety
codes**) shows who you are and who you share the workspace with, by safety
code rather than by name:

- **Your safety code**, eight groups of four characters. People compare it
  with the one they see for you, in person or on a call.
- **Everyone else** with their safety code, a **Look-alike name** tag when
  two names (or someone's and yours) look the same, and their linked accounts
  as chips (verified, self-asserted, stale, not checked). The chips show what
  your host last checked; the phone never asks a forge. A handle can't
  reorder the text around it.
- **Invites for you** that arrived in your host's contact drop box. An invite
  is *from* someone only when their device signed it and their code matches
  ("From Ada: signed by their device…"); anything else only *says* it's from
  them. An unsigned message invite can't be accepted, and a stranger's needs
  you to compare safety codes first.
- **Invite by link or QR code**: inviting a person is an Admin's act with
  a passkey, which a box (its own service member) can't do and the phone
  tier doesn't include; make the invite on your desktop. **Open an invite
  link** (paste or scan) shows who it's from before anything is joined.
- **Direct messages** appear at the top of the chat list. A DM is a
  workspace of its own where the phone's key isn't certified, so what the
  phone sends there goes as the box, not as you; hold DMs as yourself from
  your desktop. Agents don't run in DMs and DMs never send a notification.

When you join from an invite, the chat may say **Waiting for Ada's founding
to sync** until the founder's signed founding reaches your host. Send is off
and your draft is kept; it clears on its own.

A box is never your device since 0.1, so person invites and DMs as you
start on your desktop. Code-host changes are made on the host or the
desktop.

## Repos

The repo chip in a chat's header shows which repository and branch the chat
works in on your home host. Tap it to switch the chat to another checkout,
clone a repo from any code host your host is connected to (GitHub, GitLab,
Forgejo, Gitea or Codeberg; filter by host), pull the latest, or open a pull
or merge request from the chat's branch. **Settings → Code hosts** lists
your host's connections (it holds the tokens, never the phone). See [Repos on the home host](../../operate/ops/serve-repos.md).

## Reviewing proposals

Proposals (an agent's file changes, a command, a decision) appear on the
phone with the same collaboration state as the desktop's Review card:

- **Votes, with who and role.** Each member's latest vote shows their name and
  workspace role, alongside the required approvals, the qualifying approvals
  so far and whether quorum is met.
- **Approve or reject.** The phone signs the vote as you with its own key,
  bound to the content digest it shows; it counts at your role, as your
  approval. A workflow proposal or a human-gated one (a lockdown, dearth-mode
  brood paths) asks for your passkey first. Quorum, the role floor and the
  no-self-approval rule apply unchanged; a refused vote gets an error before
  the phone signs anything.
- **Apply on ‹host›.** Once approved, pick a host (from the host list) to
  apply it. The request shows as *open* until that host reports *applied*
  (with its commit) or *failed* (with the reason); you can **Withdraw** it
  while it is open. One request at a time per proposal.
- **Refinement thread.** Read the proposal's notes and add your own. Mark a
  note as a change request.
- **Request changes.** *Send to agent to revise* bundles the change requests
  (or every note, if none is marked), records it on the thread and sends the
  instruction to the agent that produced the proposal. When that agent files
  its next version in the chat, the new version is linked to the old one and
  the old one leaves the open list.
- **Rendered Markdown.** A changed `.md`/`.markdown`/`.mdx` file (or a
  `README`) has a *Diff | Rendered* switch. *Rendered* shows the document as
  formatted Markdown, with a rail beside the blocks the proposal changed, and
  *Before | After* to compare. New and deleted files come from the diff
  itself; for an edited file the phone asks the host for the whole file,
  rebuilt from the chat's checkout without touching it. When the host can't
  (no checkout bound, or the file has moved on), the phone renders only the
  changed sections and says so.

### Proposal sets

A [proposal set](proposal-sets.md) shows on the phone as one card per set:
the question, its phase (scouting, piping, decided), and each alternative
(A, B, …) with its decayed scout support against the quality bar, how many
scouts qualify toward the agent quorum, and a badge when it is the
front-runner, frozen, stopped or the winner. Scout assessments appear only
once a scout has posted them (blind until posted), and a scout rating its own
alternative is shown but never counted. The latest pre-flight result shows
per check.

You can vote on any alternative from the phone, through the same vote as any
other proposal. Pre-flight, apply and the set's decision stay on the desktop's
set view: an alternative is applied only after a passing pre-flight, never on
approval.

Approving a proposal from the phone never applies it: there is no
auto-apply. Once it is approved, **Apply on ‹host›** asks one host (your
`ligus serve` box, a worker, or a desktop) to apply it; that host, and only
that host, applies it once and reports the commit or the reason it couldn't
(see [Apply on a host](../../operate/ops/serve-box.md#apply-on-a-host)).
While the request is open nobody else applies the proposal; you can
**Withdraw** it. As on the desktop, **Implement** still applies it there.

## Workflow approvals

A workflow run that reaches an approval gate files a decision proposal for it.
It shows up on the phone like any other proposal, and approving or rejecting
it there votes on the gate exactly as the desktop's Review pane does. Workflow
proposals (an agent's suggested new workflow) appear too, as a decision with
the stages as its text. See [Workflows](workflows.md).

## Push notifications

With [remote access through a relay](../../operate/networking/remote-access.md) set up,
`ligus serve` can wake the phone with a notification. The content is end-to-end
encrypted between the box and the phone: the relay and Apple see only
ciphertext.

| Notification | When |
|---|---|
| *Name* mentioned you | a message from an agent or another member `@mentions` you, `@all`, a role group you're in, or an agent's `@you` in a reply to you |
| *Workflow* needs approval | a workflow run's approval gate is waiting (again on each rerouted attempt) |
| *Agent* replied / hit an error | an agent reply **to you** finished: one answering a message you wrote (or a hand-off or workflow you started). Replies to other people don't notify you unless they mention you. Not a reply that already notified you as a mention |
| Change ready for review | a proposal is waiting for review (a workflow gate is announced once, as an approval, not also here) |
| The review queue needs you | the review queue backed up and a desktop with *Recruit reviewers* on asked you to help; tapping opens the proposal that has waited longest for you |
| New phone paired | a new phone was paired with the box |

Tapping a notification opens its chat. Notifications offer **Approve** and
**Reply** actions where they apply; a reply that can't reach the box
within 20 seconds is queued.

- **Previews are on by default.** A notification shows who did what as the
  title and, for replies and mentions, the first 120 characters of the message
  as the body. Markdown is flattened to plain text first: a table becomes
  `Header: value` lines, list items get a bullet, and headings, fences and
  link URLs are dropped.
- **Previews are per phone.** Turn them off in the app's notification
  settings and that phone gets only the title ("coder replied"). Other phones
  keep their own setting.
- **No duplicates while you're looking.** While the app is open and following
  the live event stream, the box doesn't push to that phone.
- **Only news.** Rapid bursts in one chat collapse into one notification per
  10 seconds, there are per-phone and host-wide rate limits, and history that
  synced in late (older than 10 minutes when `ligus serve` started) never
  pushes.

!!! note "Push is off in standard builds"
    The app's push support is compiled in but switched off by default
    (`LIGUS_PUSH_NOTIFICATIONS = NO` in its build settings). To get push you
    build the app yourself with it switched on. Without push the app still
    shows everything when you open it.

!!! warning "Push needs your own APNs key today"
    Apple accepts a push only when it is signed with a key from the team that
    signed the app. With a self-hosted relay, that means running the relay
    with your own APNs key and building the app under the same Apple team. See
    [Remote access → Push through your own relay](../../operate/networking/remote-access.md#push-through-your-own-relay).

Mentions also work on the desktop; see
[Mentions & notifications](mentions-and-notifications.md).

### Troubleshooting push

Start with a test notification from the box:

```sh
ligus remote push-test            # every paired phone; --phone <pid|name> for one
```

```text
Sending a test notification through https://<relay-url> …
1. this box:  host id <host id>
2. relay:     https://<relay-url> (connected, push available)
3. phone "iPhone" (<pid>):
   on this box: notifications on (previews on); app open right now: no
   box → relay: sent (encrypted for this phone)
   relay → APNs: apns sandbox topic com.example.ligus.dev: 200 (apns-id …)
   => APNs accepted it. …
```

The test goes out even while the app is open (real notifications don't), and
the command exits non-zero when any phone's push didn't get through. What the
failures mean:

| `push-test` says | Cause | Fix |
|---|---|---|
| `relay: … NOT connected` | the box can't reach the relay | `ligus remote status`, and the box's log |
| `push UNAVAILABLE` | the relay has no APNs key | [Push through your own relay](../../operate/networking/remote-access.md#push-through-your-own-relay) |
| `notifications OFF` | the app never asked this box for notifications: push is off in the app, iOS permission was never given, or the app was reinstalled (or its bundle id changed) and hasn't asked again | in the app, **Settings → Notifications** on, and allow notifications when iOS asks |
| `refused: no_push_token` | the relay has no APNs token for the phone: it never registered one, the relay restarted without durable storage, or APNs said the token was dead | open the app with notifications allowed; it registers its token every time it connects |
| `topic not allowed on this relay` | the app's bundle id isn't one the relay pushes to | add it to `LIGUS_RELAY_APNS_TOPICS` on the relay and restart it |
| `BadDeviceToken` | environment mismatch: a build run from Xcode has a sandbox token, an App Store or TestFlight build a production one | `LIGUS_RELAY_APNS_ENV=auto` (the default) on the relay; open the app again |
| `DeviceTokenNotForTopic` / `TopicDisallowed` | the topic the relay used isn't the app on the phone | set `LIGUS_RELAY_APNS_TOPIC` / `LIGUS_RELAY_APNS_TOPICS` to the build's bundle id, and update the app (current builds report their bundle id) |
| `InvalidProviderToken` | the relay's APNs key, Key ID or Team ID is wrong, or from another team than the app | fix the relay's `LIGUS_RELAY_APNS_KEY*` / `TEAM_ID` |
| `Unregistered` (410) | the app was uninstalled or reinstalled | open the app so it registers a new token |
| `no answer from the relay` | the relay didn't ack within 15 s | the relay's log (`tunnel push` lines) |
| `accepted` but nothing on the phone | APNs took it; the phone didn't show it | iOS **Settings → Notifications → Ligus**, Focus modes, and open the app once since installing (it stores the key its notification extension decrypts with) |

If the test arrives but real notifications don't, read the box's log. Every
notification is logged once per phone, without its content:

```text
push: message.done to "iPhone" (Ab3dE9…) sent
push: message.done to "iPhone" (Ab3dE9…): relay accepted it
push: mention to "iPhone" (Ab3dE9…) not sent: the app is open on the phone
push: message.done to "iPhone" (Ab3dE9…): relay refused it: no_push_token (…)
```

- **`the app is open on the phone`** means the phone has the app's live event
  stream open and has talked to the box in the last 40 seconds. The app closes
  its stream when it goes to the background; if iOS suspends it first (screen
  locked at once), the box stops counting it as open after 40 seconds of
  silence. Older app builds could leave it counted for up to a minute more.
- **`coalesced` / `rate-limited`**: bursts in one chat collapse to one
  notification per 10 seconds; 20 a minute and 500 a day per phone.
- **No `push:` line at all** for an agent reply: the box never saw a finished
  reply to notify about. Replies and mentions that synced in late (older than
  10 minutes when `ligus serve` started) don't notify, nor does your own
  message, nor anything in a direct message, nor a reply to someone else
  (one that answers another member's message and doesn't mention you).

On the relay, every push is one `tunnel push` log line with `result` (`sent`,
`no_push_token`, `push_failed`, `rate_limited`, `revoked`, `push_unavailable`)
and, for APNs, `env`, `topic`, `status`, `reason` and `apns_id`. It never
logs the device token or the notification.

## Workspace guards

The box exposes the workspace's guard state at `GET /v1/guards`: whether
[dearth mode](guards.md#dearth-mode) is on and why, which chats are
[locked down](guards.md#chat-lockdown) and by what kind of signal, and which
host holds the workspace coordinator lease. The app shows a dearth-mode banner
and a lockdown bar on a locked chat. A workflow run in the chat's details names
the device coordinating it.

**Clearing a lockdown from the phone.** The lockdown bar has **Clear** when
you are a human member at Contributor or above. The phone signs the same
`chatLockdownCleared` event the desktop's *Clear lockdown* writes, as you,
with its own key and always with your passkey (the `clearLockdown` phone
action); anyone else gets an error and nothing is written. The clear names
the lockdown signals the bar showed you: a signal raised after you looked
stays in force until someone clears it too, however long the box holds the
clear. Switching dearth mode and approving joins stay on the desktop.

## Flow

`GET /v1/flow` gives the phone a compact, read-only view of
[Flow](flow.md): the task board (open work and who claimed it), the review
queue with its backpressure alert, active workspace marks, and a summary of
the routing shadow report. The thresholds are the defaults (queue limit 5,
long wait two hours), since a desktop's Flow settings are its own. Hand-off
cards appear in the chat at the reply that handed the work on
(`GET /v1/chats/{id}/handoffs`). Picking up board work, marks, zones and route
stops stay on the desktop.

## Memory and task state

The app reads the [workspace memory](memory.md) (`GET /v1/memory`, search
with `?q=`) and each chat's live task state
(`GET /v1/chats/{id}/task-state`), and can propose a note, an edit or a
removal with the `proposeMemory` action, signed by the phone's own key as
you. A person accepts or rejects it on a desktop. A new note is named by its
proposal, so the app's own `entryId` for a create isn't used.

## Appearance

The app can follow the box's theme. Pick **Match host** in the app's
appearance settings and it reads `GET /v1/appearance` from the box, which
answers a palette (`pollen`, `studio`, `harbor`, `slate`, `meadow` or
`obsidian`) and a mode (`auto`, `light` or `dark`).

The box decides each value on its own, in this order:

1. its own settings, `LIGUS_SERVE_PALETTE` and `LIGUS_SERVE_APPEARANCE`;
2. the desktop app's saved theme, if the desktop app is installed on the same
   machine (read on every request, so a change on the desktop shows up the
   next time the phone asks);
3. neither: the app uses its default palette.

## Dashboard

The box can also serve a dashboard for the phone: a built-in Ligus section
(agents, proposals, workflow runs, turns, the host's status) plus panels you
define in `dashboard.toml` on the box. The app hides its dashboard tab when
the dashboard is empty. Format and examples:
[`docs/dashboard.md`](https://github.com/langstroth-ai/ligus/blob/main/docs/dashboard.md).

## Phone-signed actions {#phone-signed-actions}

The phone has its own P-256 key in the Secure Enclave, certified by your
desktop with your passkey. What you do as yourself from the phone goes
through it:

| On the phone | Signed event | Passkey |
|---|---|---|
| Post a message | a user message, as you | no |
| Approve or reject | a vote bound to the content digest | for a workflow proposal or a human-gated one |
| Answer an agent's question | the answer, as you | no |
| Clear a lockdown | `chatLockdownCleared` | always |
| Apply on ‹host› | an apply request naming that host | when the proposal is human-gated |
| Withdraw an apply request | the withdrawal | no |

For each, the box drafts the exact event and the phone reads it before
signing: it signs only if the bytes are exactly what you asked for, in this
workspace and chat, as you, from this phone, with the passkey confirmation
you just gave (or none, when none was asked for). The confirmation sheet
shows what the phone read from those bytes. The box can delay or drop an
action; it can't change one. A phone signs nothing else: no roles, removals,
invites, rules or agent moves (admitting a box is the one membership change,
see [Admitting a box](../../operate/ops/admitting-a-box.md)). The wire is in
the [serve API reference](../../operate/reference/serve-api.md#phone-actions).

## What stays on the desktop

The phone is for following, deciding and steering. These need the desktop
app:

- creating agents and editing runtimes, and moving an agent to another
  host (an Admin's act the phone tier doesn't include);
- managing skills, vaults, MCP servers and settings;
- applying a proposal-set alternative (you can vote on it from the phone);
- switching dearth mode and approving joins (the phone clears a chat
  lockdown with your passkey);
- answering `@hive`: only a desktop runs a chat's primary runtime;
- removing or re-roling members, and issuing invite links to people (the
  phone admits a box, nothing else about membership);
- direct messages: the phone's key isn't certified in a DM's own workspace;
- joining another workspace (a host serves one; DMs are the exception).

The full API the phone uses is in the [serve API reference](../../operate/reference/serve-api.md).

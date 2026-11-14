# Ligus on your phone

The Ligus iPhone app is a thin client for one person's workspace. The phone
doesn't run Ligus and holds no workspace keys. It talks to **`ligus serve`**, a
Ligus device you run on an always-on box, over a small JSON + Server-Sent
Events API (`/v1/…`). The box keeps the end-to-end encryption keys, syncs with
your relay and runs the agents you send to it; the phone reads and writes
through it.

> The app's source lives in its own repository. The `ligus serve` side and the
> API contract the app uses are in this repo
> ([`docs/ios-app.md`](https://github.com/ligus-ai/ligus/blob/main/docs/ios-app.md)),
> so any client can implement them.

To set up the box and pair a phone, see
[Remote access for your phone](../networking/remote-access.md). To make the
box act as you rather than as a separate member, see
[Running ligus serve as your own device](../ops/serve-box.md). For the whole
setup end to end, see [Personal fleet](../deploy/personal-fleet.md).

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
You can also add a host by typing its address; the app assumes
`http://…:8787`.

Each host profile has a **path** setting:

- **Auto (direct, then relay)**: use the direct address when it answers
  quickly, otherwise the relay;
- **Direct only**;
- **Relay only**, even when the direct address would answer.

The app can hold several host profiles, each stored as its own Keychain
item. Credentials live in the iOS Keychain; the phone never holds a
workspace key or a device key.

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
[revoking a lost phone](../security/runbooks.md#lost-phone).

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
    [your own device](../ops/serve-box.md).

You can also move an existing agent to a different host from the phone.
Creating agents and editing runtimes stay on the desktop.

## Repos

The repo chip in a chat's header shows which repository and branch the chat
works in on your home host. Tap it to switch the chat to another checkout,
clone a GitHub repo, pull the latest, or open a pull request from the chat's
branch. See [Repos on the home host](../ops/serve-repos.md).

## Reviewing proposals

Proposals (an agent's file changes, a command, a decision) appear on the
phone with the same collaboration state as the desktop's Review card:

- **Votes, with who and role.** Each member's latest vote shows their name and
  workspace role, alongside the required approvals, the qualifying approvals
  so far and whether quorum is met.
- **Approve or reject.** The phone votes through the same core call as the
  desktop, at the box's real roster role. Quorum, the role floor and the
  no-self-approval rule apply unchanged; a role below the floor gets an error
  and nothing is written.
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

A proposal that is an alternative in a [proposal set](proposal-sets.md)
carries its `setId`. You can vote on it from the phone, but it is applied only
from the desktop's set view, after a pre-flight, and never on approval.

Approving a file-diff proposal from the phone doesn't apply it, unless the
box is set to (see [`LIGUS_SERVE_APPLY_ON_APPROVE`](../ops/serve-box.md#apply-on-approve)).
As on the desktop, **Implement** applies it.

## Workflow approvals

A workflow run that reaches an approval gate files a decision proposal for it.
It shows up on the phone like any other proposal, and approving or rejecting
it there votes on the gate exactly as the desktop's Review pane does. Workflow
proposals (an agent's suggested new workflow) appear too, as a decision with
the stages as its text. See [Workflows](workflows.md).

## Push notifications

With [remote access through a relay](../networking/remote-access.md) set up,
`ligus serve` can wake the phone with a notification. The content is end-to-end
encrypted between the box and the phone: the relay and Apple see only
ciphertext.

| Notification | When |
|---|---|
| *Name* mentioned you | a message from an agent or another member `@mentions` you, `@you`, `@all` or a role group you're in |
| *Workflow* needs approval | a workflow run's approval gate is waiting (again on each rerouted attempt) |
| *Agent* replied / hit an error | an agent reply finished (not your own messages, and not a reply that already notified you as a mention) |
| Change ready for review | a proposal is waiting for review (a workflow gate is announced once, as an approval, not also here) |
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
    [Remote access → Push through your own relay](../networking/remote-access.md#push-through-your-own-relay).

Mentions also work on the desktop; see
[Mentions & notifications](mentions-and-notifications.md).

## Workspace guards

The box exposes the workspace's guard state read-only at `GET /v1/guards`:
whether [dearth mode](guards.md#dearth-mode) is on and why, which chats are
[locked down](guards.md#chat-lockdown) and by what kind of signal, and which
host holds the workspace coordinator lease. Switching dearth mode, approving
joins and clearing a lockdown stay on the desktop for now.

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
[`docs/dashboard.md`](https://github.com/ligus-ai/ligus/blob/main/docs/dashboard.md).

## What stays on the desktop

The phone is for following, deciding and steering. These need the desktop
app:

- creating agents and editing runtimes (moving an agent to another host
  works on the phone);
- managing skills, vaults, MCP servers and settings;
- applying a proposal-set alternative (you can vote on it from the phone);
- applying a file-diff proposal, unless the box sets
  `LIGUS_SERVE_APPLY_ON_APPROVE=1`;
- switching dearth mode, approving joins, clearing a chat lockdown (the
  phone shows guard state read-only);
- answering `@hive`: only a desktop runs a chat's primary runtime;
- inviting, removing or re-roling members.

The full API the phone uses is in the [serve API reference](../reference/serve-api.md).

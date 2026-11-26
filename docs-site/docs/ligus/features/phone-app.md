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

## Finding your way {#navigation}

The app has four tabs: **Inbox**, **Chats**, **Review** and **Agents**.
Settings opens from your avatar at the top right of each tab, and **Done**
takes you back.

![The phone app's Chats tab: direct messages, channels and chats that need you (demo workspace)](../images/phone-chats-light.png#only-light){ width="300" loading=lazy }
![The phone app's Chats tab: direct messages, channels and chats that need you (demo workspace)](../images/phone-chats-dark.png#only-dark){ width="300" loading=lazy }

- **Inbox** is home. *Needs you* lists only what waits on you: votes you can
  still cast, approval gates, agents' questions and permission requests you
  may answer, this phone needing to be added to your devices again, a box
  waiting for your approval, invites, and failures of runs you started.
  Deadlines come first. Each row opens its chat or proposal; nothing is
  approved or answered from the list. Below it, *Running* shows chats with an
  agent working or queued, and *Done today* (folded) what finished today.
  The Inbox count is the only badge, on the tab and on the app icon (when
  you allowed badges); unread messages are a dot on the chat row.
- **Chats** and **Agents** rows say where each chat or agent stands, with a
  glyph and a word: *Needs you*, *Failed*, *Paused*, *Working*, *Queued* or
  *Done* (finished since you last looked). The desktop's chat rows use the
  same words.
- **Review** has a filter: *All*, *Needs my vote*, *Waiting on others*,
  *Mine* and *Workspace* (the review queue, task board and notes;
  see [Flow](#flow)).
- **Agents → Health** is the box's dashboard ([Dashboard](#dashboard)).

## Add your phone {#add-this-phone}

(For a second computer, the same idea without a QR code: [Add a
computer](linked-devices.md).)

Before the phone can pair with your box or sign anything, your desktop
certifies the phone's own device key (a P-256 key in the Secure Enclave) as
one of **your** devices. Since 0.1.1 the desktop shows a QR code and the
phone scans it with the Ligus app; the phone's code reaches the desktop by
itself, through your `ligus serve` box.

1. **On the desktop:** **Settings → Profile → My devices → Add a phone →
   Show code**. The QR code appears with a 5-minute countdown. It is hidden
   when Ligus isn't in front (the code stays valid); choose **Show code** to
   bring it back, and **Show a new code** once it expires. If this desktop
   can't show a code (the workspace isn't shared, it has no relay
   connection, the workspace has no box, or your account has no passkey or
   recovery key), the panel lists each missing piece with a link to the fix
   ("No box in this workspace → **Admit a box**", "→ **Add a passkey**",
   "→ **Open Team & sync**") and offers [Can't scan?](#cant-scan) instead.
   If the workspace has a box that hasn't been seen for a while, the panel
   says so above **Show code** but still lets you try. If the check itself
   couldn't run, the panel says so with **Check again**. **Add a phone…** is
   also in the command palette (⌘K).
2. **On the phone:** **Settings → This phone → Add to my devices**. If the
   phone would call itself just "iPhone" (iOS doesn't let apps read the
   name you gave your iPhone), the app first asks you to **Name this
   phone**: type a name your desktop will show, such as "Ana's iPhone", and
   choose **Save name**. Then point the camera at the desktop's code. Use
   the scanner inside the Ligus app; the phone's camera app doesn't read
   this code.
3. The phone asks the box who showed the code and says "Add this phone to
   *Ana* (*role*)'s devices in *workspace*?". Check that it is you, then
   choose **Add**. The phone then shows its fingerprint
   (`XXXX-XXXX-XXXX-XXXX-XXXX`, five groups of four) and waits.
4. **On the desktop:** a card says "*Ana's iPhone* wants to be added",
   "Relayed by *box*", who the phone will act as, and the fingerprint. **Compare the
   fingerprint with the phone's screen**, group by group, and tick "Your
   phone shows the same fingerprint". The phone's name is only what the
   phone calls itself; the fingerprint is what proves it is the phone in
   your hand.
5. Choose **Add this phone** and confirm with your passkey; the passkey
   sheet names the same fingerprint. The button stays disabled for the first
   5 seconds after the request appears ("Listening for other phones…"): if
   a second phone answers the same code in that time, nothing is added and
   the desktop asks you to start again. If the card isn't your phone, choose **Not my
   phone**.
6. The phone sees that the box now knows its certificate, pairs, and says
   "This phone is now one of your devices". There is nothing to retry by
   hand.

**My devices** lists the phone once, under the name it gave itself
(cleaned, as every name is). That name is remembered only on the desktop
that added the phone, in its local settings. Your other desktops show the
name the phone's box knows it by, or "Device" and the first 8 characters of
its device id (for example "Device 6f1c2a9b"). Both flows
([Can't scan?](#cant-scan) too) remember it, and adding a phone that is
already yours again replaces the old name rather than adding a second row:
the phone says "This phone is already one of *Ana*'s devices in
*workspace*. Update its name there?" and **Update name** sends its current
name to the desktop, which needs no passkey for it. When one of your own
boxes (a box enrolled as your device) has the phone paired, the row also
says so: "Paired with home-server · seen 2 min ago". A box reports only its
owner's phones, and your desktop ignores what any other device says about
yours.

The phone is added to the workspace the desktop showed the code for, under
the account of the person at that desktop. The phone warns you before
**Add** when two people in the workspace share the name shown ("Two people
here are called Ana. Make sure this is your desktop.") and when this phone
already acts as someone else on another host profile ("This code is from a
different account.").

### A phone that isn't paired yet {#new-phone}

A brand-new phone has no channel to your box yet, and the box pairs only
phones it already knows as certified. The flow handles this through the
box:

- If the phone has no host profile for the workspace in the code, it says
  "Pair with this workspace's box first" and opens the pairing scanner,
  keeping the desktop's code in memory until it expires. Scan the QR code
  `ligus pair` prints on the box.
- Through the relay, the box answers an uncertified phone with a
  **provisional** connection: it reaches the three add-a-phone routes and
  nothing else, for at most 5 minutes. Once the desktop has certified the
  phone and the box has synced, the phone closes it and pairs normally with
  the same link.
- On the direct path (LAN or tailnet), the phone is paired before it is
  certified and calls the same routes with the box's token.
- If pairing was refused first ("add this phone first"), the refusal screen
  leads with **Scan your desktop's code**.

A pairing link allows five attempts in all; the provisional connection and
the final pairing each use one. If the link runs out, run `ligus pair` again.

### Can't scan? {#cant-scan}

The paste flow still works, for a phone without a camera, a desktop that
can't show the code (no relay connection, or no passkey on this account), or
a box older than 0.1.1:

1. In the app, open **Settings → This phone → Can't scan? Show this phone's
   code**. A phone still called just "iPhone" is asked to name itself
   first (**Save name**). It then shows the phone's code (`ligusphone1:…`,
   as a QR code and as text) and its fingerprint.
2. On the desktop, under **Add a phone**, open **Can't scan?** and paste or
   type the code. The phone's copy is local to the phone and never goes
   through the shared clipboard, so on a Mac you either type it or move it
   some other way you trust.
3. Compare the fingerprint, tick the box, choose **Add this phone** and
   confirm with your passkey. If you switch workspaces before confirming,
   the desktop refuses and asks you to check the code again.
4. Give the box a few seconds to sync, then tap **Retry** on the phone (or
   scan a new `ligus pair` link if the old one used its five attempts).

Without a desktop, `ligus phones certify` does the same on a CLI identity of
yours with your recovery key ([CLI reference](../reference/cli.md)).

### What the phone can do once added

A phone code can't take over a device. A phone's device id is derived from
its key, so a code naming any other id (one of the workspace's desktops or
boxes, another phone, or an id the phone chose) is refused by the desktop
before it shows the fingerprint, and a certificate for it by every member's
fold. Both flows end in the same certificate, checked by the same rules.

The phone is certified at the **phone tier**: it can post, react, vote,
answer an agent's question, clear a lockdown and ask a host to apply, as you
and capped by your role; it never changes members, devices or keys.

After pairing, the app asks the box `GET /v1/me` and fills in the workspace,
your account, your role and the passkey relying party by itself.

### The workspace's name {#workspace-name}

The phone shows the workspace under the name you gave it on the desktop:
the Chats header shows the workspace's name, and the host switcher and
**Settings → This phone** show it with the box it goes through after it,
"Comb Council · via home-server".
A desktop keeps a workspace's name in its own settings and in the invites
it makes; a box joins from an admission and never sees one. So the
desktop of the workspace's Owner (or an Admin) also writes the name to the
workspace's config log, once, the next time it opens the workspace, and
the box gives it to the phone (`workspaceName` in `GET /v1/me` and in the
add-a-phone check). Until that has synced to the box, the phone shows the
box's name only. A name already on the config log is kept. Only an Owner or
an Admin can name or rename the workspace: every device drops a
Contributor's or Viewer's name on the config log, so a member can't make
the phone ask to be added to someone's devices "in" a workspace that looks
like another one. The name is cleaned like any other name and shown up to
120 characters.

### If adding the phone fails {#add-phone-troubleshooting}

| The app or desktop says | Why | What to do |
|---|---|---|
| "Show a new code on your desktop and scan again" | the code is older than 5 minutes | **Show a new code** on the desktop and scan it |
| "This code expires too far ahead: check the clock" | the phone's or the box's clock is off by several minutes | set the clocks automatically, then show a new code |
| "Pair with this workspace's box first" | the phone has no host profile for this workspace | scan `ligus pair` from the box (see [A phone that isn't paired yet](#new-phone)) |
| "This box can't reach the workspace's relay. Use Can't scan?" | the box syncs only peer to peer, or it hasn't been admitted to the workspace yet, so it can't pass the request to the desktop | use [Can't scan?](#cant-scan), give the box a relay, or [admit the box](../../operate/ops/admitting-a-box.md) first |
| "Update ligus serve, or use Can't scan?" | the box is older than 0.1.1 | update the box, or use [Can't scan?](#cant-scan) |
| "this code is for another workspace" | the phone is paired with another workspace's box | pair with this workspace's box, or show the code from the right workspace |
| "this phone belongs to someone else here" | the phone is already certified for another person in this workspace | [reset this phone's key](#revoked-phone) and add it again |
| "This phone was revoked here … can't be added again with this key" | this phone's device key was revoked in My devices | [reset this phone's key](#revoked-phone) and add it again |
| the desktop declined | **Not my phone** was chosen | show a new code and scan again |
| "Two phones answered this code. Someone else may have seen it. Nothing was added." (desktop) | a second phone answered before you approved | **Show a new code**, keep it off shared screens and out of sight of cameras, and scan again |
| "this box is taking too many add-a-phone requests; try again in a minute", or the box is busy with other phones (pairing closes with `4043 provisional_busy`) | the box takes 30 requests a minute and 4 unpaired phones at once | wait a minute and scan again (show a new code if it expired) |
| "Too many attempts" (close `4029`), or the phone keeps waiting | the relay is rate limiting your network (failed attempts per IP address, per /64 on IPv6) | stop retrying and wait 5 minutes; then `ligus pair` for a new link if needed, show a new code and scan it once |

### "Another phone answered this code" {#another-phone}

If someone else saw or photographed the desktop's QR code, they could answer
it from their own phone. The desktop keeps listening until the code expires,
even after you approve, so that this can't happen silently:

- **On the desktop:** "Another phone answered this code after you added
  *label* (fingerprint …). If the phone you added isn't the one in your
  hand, revoke it now." It also shows the other phone's name and
  fingerprint. Compare both with the fingerprint on your phone
  (**Settings → This phone**). If they differ, choose
  **Revoke *label***: it is your own device, so your passkey is enough. If
  they match, choose **It's mine**.
- **On the phone:** "Another phone was added with this code. Tell the
  person at the desktop to revoke it." Your phone wasn't added; someone
  else's was. Revoke the added phone on the desktop
  (**Settings → Profile → My devices → Revoke**), then show a new code and add
  your phone again.

If the phone you revoked had already paired with your box, also revoke its
pairing on the box (`ligus phones revoke`; see
[Lost phone](../../operate/security/runbooks.md#lost-phone)). Then check
**My devices** for any other phone you don't recognise.

### Revoking a phone, and getting it back {#revoked-phone}

Revoking a phone in **My devices** is permanent **for its key**. A phone's
device id is derived from its Secure Enclave key, and that key survives
deleting and reinstalling the app, so the same phone can't be added again
until it has a new key. Every box that paired it cuts the pairing.

Before you revoke, check the row: My devices lists each device once, says
which box a phone is paired with and when it was last seen, and the
confirmation names it ("This is the iPhone paired with home-server right
now; it will be signed out of this workspace and can't be added back with
its current key"). If that is the phone in your hand, you probably don't
want to revoke it.

To bring a revoked phone back:

1. **On the phone:** on the screen that says this phone was revoked (the
   pairing refusal, or "This phone was revoked here" in Add to my devices),
   choose **Reset this phone's key**. A phone with no workspace has the same
   action as a small link, **This phone was revoked? Reset its key**, on its
   first screen and on the Connect screen; Settings isn't needed. On a
   phone that is still connected somewhere, it is under **Settings → This
   phone → Advanced**.
2. Read the sheet ("Reset this phone's key?") and choose **Reset key with
   Face ID**, then pass Face ID (or your passcode). The phone deletes its
   old key, makes a new one and gets a new device id. It also replaces its
   pairing keys (the Noise static and auth key it pairs through a relay
   with): a box refuses a pairing made with the keys of a revoked device's
   pairing, whatever device it claims, so keeping them would keep the phone
   refused (`4041 device_revoked`, naming the old device id). Its pairings
   through a relay end; a direct pairing stays. Anything signed with the
   old key stays in the workspace's history, and the old key stays
   revoked. The app never does this by itself, and refuses on an iPhone
   with no passcode.
3. The sheet then opens **Add to my devices** (naming the phone first if
   it is still called just "iPhone"): add it again from your desktop as in
   [Add your phone](#add-this-phone). It is a new device to every workspace
   it was in, so add it on each one, and pair with each box again with a
   **new** code from `ligus pair` (a pairing link the phone already tried is
   bound to its old keys).

!!! note "Reset on an older app, still refused?"
    Before Ligus for iPhone fixed this, a reset replaced only the device
    key, so the box kept refusing the phone as the revoked device (its log
    shows `device_revoked … workspace device <old id>` on every try). Update
    the app, reset the key once more, then add the phone and pair with a new
    `ligus pair` code.

## Pairing: direct or through the relay

The app connects to a box in one of two ways, and a host profile can use
both:

| Path | How | What the phone stores |
|---|---|---|
| **Direct** | plain HTTP to the box's address (`http://<host>:8787`) on your LAN or tailnet, with the box's bearer token | the URL and `serve.token` |
| **Relay** | a Noise session through your relay's tunnel; the relay forwards ciphertext only | the box's host id and public key, the relay URL, and the phone's own key and credential (90 days, renewed daily while used) |

Pair by scanning the QR code `ligus pair` prints (or opening its
`ligus://pair?…` link): the Connect screen shows the command to run on the
box and one **Scan the code** button, with pasting a link or typing an
address under **Other ways to connect**. Before it pairs, the app says what
pairing grants: the phone reads your chats and proposals through the box,
posts, votes and answers as you (signed by its own key once it is one of
your devices), and never holds your workspace keys. A **v2** link carries the relay
details as well as, when there is a usable direct address, the URL and token.
A box listening only on loopback (`127.0.0.1`) with remote access off prints
no link, since the phone can't reach it; see
[Remote access](../../operate/networking/remote-access.md).
You can also add a host by typing its address; the app assumes
`http://…:8787`.

Each host profile has a **path** setting:

- **Auto (direct, then relay)**: use the direct address when it answers
  within 1.5 seconds, otherwise the relay, and check again when the network
  changes;
- **Direct only**;
- **Relay only**, even when the direct address would answer.

The app can hold several host profiles, each stored as its own Keychain
item. Credentials live in the iOS Keychain; the phone never holds a
workspace key. Its own device key lives in the Secure Enclave and never
leaves it ([Add your phone](#add-this-phone)).

## Host path policy

What the phone can reach on the box's disk is deliberately narrow:

- **Checkouts** come only from the box's repos dir (`LIGUS_REPOS_DIR`,
  default `<data dir>/repos`). A chat can be bound only to a plain directory
  name directly inside it that is already a git repository. Bindings to
  other paths are made on the box with `ligus bind-repo`; the phone sees only
  their last path component.
- **Clones** accept only `owner/name` (`group/name` on GitLab) or
  `https://<host>/owner/name`, where the host must be one of the box's
  code-host connections; other schemes, credentials in URLs, queries and
  fragments are refused. A github.com repository lands in
  `<repos dir>/<owner>__<name>`, one from another host in
  `<repos dir>/<host>__<owner>__<name>`. Clones are limited by
  `LIGUS_REPOS_MAX_CLONE_MB` (2048) and `LIGUS_REPOS_CLONE_TIMEOUT_SECS`
  (900), two at a time.
- **Proposal files** (`GET /v1/proposals/{id}/file`) take a plain relative
  path: no `..`, no absolute paths, nothing under `.git`. The file is rebuilt
  in a scratch directory, never in the checkout.
- **Code-host** actions use the box's connections: its own `gh` login for
  GitHub by default, or tokens in the box's OS secret store, never on the
  phone. Ligus never force-pushes,
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

**Explore the demo** on the welcome screen opens a sample workspace that runs
entirely on the phone, with no box or relay. Its card (with *Connect your
box* and *Exit demo*) is on the Inbox; the other tabs say "Demo workspace ·
sample data" in one line. Replies come from Apple's
on-device model where available, otherwise from scripted text. Nothing is
sent anywhere, and there's no push in the demo.

## Chats

- The chat list shows each chat's title, a preview of the newest message, an
  unread count and up to five recent participants. Chats are filed under their
  channels, in the desktop's sidebar order.
- Start a chat or send a message from the phone. `@mentions` route exactly as
  on the desktop: `@agent` wakes that agent, `@ligus` the chat's primary
  runtime. See [Multi-agent collaboration](multi-agent.md). Someone else's
  agent answers you only if they opened it to everyone; otherwise you get a
  short reply saying so ([Who can use your agents](agent-access.md)).
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
- **Notes above the composer** (why sending is waiting, signed actions not
  yet published, the demo's note) fold into one line when there are several,
  or when the text is at an accessibility size; tap it to read them. Who
  answers a message that mentions nobody is the small menu beside the
  message field.
- **Which device answered.** A reply from the chat's primary `@ligus` says
  where it ran: "This device" (the box), "Your other device", or "<name>'s
  device".
- Unread counts and read marks belong to the box, not to each device:
  opening a chat on the phone marks it read there, and your desktop keeps its
  own read state.

## People, channels and chat details

- **People** lists the workspace's human members with their role, title and
  avatar. The box's own identity (its service member) is marked "this
  host".
- **Channels** shows the channel tree, including each channel's drop-in chat.
- **Chat details** is a read-only view of the desktop's right-hand panel: who
  is in the chat, which agents it uses and where each one runs, who answers an
  untagged message (and whether that device is online right now), a rough
  context-window estimate, recent workflow runs, and the chat's skills and
  vaults.

!!! note "Who answers `@ligus`"
    Only a desktop runs a chat's primary runtime (`@ligus`): the desktop of
    the person who created the chat. Chat details says whether that desktop
    is available. If it's closed, the turn waits and is answered when the
    desktop opens. A chat the phone creates is the box's own (the box is
    its own member), so no desktop runs its `@ligus`: mention an agent
    there, or set `LIGUS_SERVE_DEFAULT_AGENT` on the box to send untagged
    messages to that agent ([Running ligus serve](../../operate/ops/serve-box.md)).

Moving an agent to another host, creating agents and editing runtimes stay
on the desktop.

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
- **Direct messages** appear at the top of the chat list: only the box's
  DMs you are in (none, if the box can't tell whose phone it is). A DM is a
  workspace of its own where the phone's key isn't certified, so what the
  phone sends there goes as the box, not as you; hold DMs as yourself from
  your desktop. Agents don't run in DMs and DMs never send a notification.

When you join from an invite, the chat may say **Waiting for Ada's founding
to sync** until the founder's signed founding reaches your host. Send is off
and your draft is kept; it clears on its own.

A box is never your device, so person invites and DMs as you start on your
desktop. Code-host changes are made on the host or the
desktop.

## Repos

The repo chip in a chat's header shows which repository and branch the chat
works in on your home host. Tap it to switch the chat to another checkout,
clone a repo from any code host your host is connected to (GitHub, GitLab,
Forgejo, Gitea or Codeberg; filter by host), pull the latest, or open a pull
or merge request from the chat's branch. **Settings → Code hosts** lists
your host's connections (it holds the tokens, never the phone). See [Repos on the home host](../../operate/ops/serve-repos.md).

**New scratch repo**, the first choice in the picker, has your home host
create an empty repository for the chat (one empty commit on `main`); the
chip shows a small **scratch** tag. **Keep it as a repo** moves it into the
host's repos folder, and then, only if you tap **Publish to GitHub
(private)** and confirm the account and name, creates a private repository
and pushes it. An unkept one is removed after 14 days without activity, with
a notice in the chat two days before. See
[Scratch repositories](repositories.md#scratch).

## Reviewing proposals

Proposals (an agent's file changes, a command, a decision) appear on the
phone with the same collaboration state as the desktop's Review card:

![A proposal on the phone: its votes (one of two approvals), what Implement does, the diff, and Reject or Approve (demo workspace)](../images/phone-proposal-light.png#only-light){ width="300" loading=lazy }
![A proposal on the phone: its votes (one of two approvals), what Implement does, the diff, and Reject or Approve (demo workspace)](../images/phone-proposal-dark.png#only-dark){ width="300" loading=lazy }

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
- **Archive.** A settled proposal (applied, rejected or replaced) has
  **Archive** in its menu: it leaves the inbox for everyone in the workspace,
  signed by the phone like a vote. Archived proposals (by a person, or after
  the workspace's auto-archive days) are out of the lists and badges and
  listed apart under **Archived proposals**, where **Restore** brings one
  back. See [Archiving proposals](right-rail.md#archiving-proposals).
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
auto-apply, for a workflow proposal either (its workflow is added when
someone presses **Implement** on a desktop). Once it is approved,
**Apply on ‹host›** asks one host (your
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

Tapping a notification opens its chat or proposal. Notifications offer
**Review** (it opens the proposal; nothing is approved from a notification)
and **Reply** where they apply; a reply that can't reach the box within 20
seconds is queued.

- **What notifies.** In the app's notification settings, *Notify me about*
  has three switches, all on: *Votes, questions and approvals for you*,
  *Mentions and direct messages*, and *Runs you started*. Other chat
  messages never notify. The phone sends the box the kinds it wants, but
  boxes don't filter by them yet, so for now a box sends every kind above.
- **How loudly.** A run that finished arrives silently and groups with other
  finished runs; a failure alerts you. Something waiting on you is
  time-sensitive (it can break through a Focus) only when it has a deadline
  within the hour. In iOS Settings → Focus, a Ligus filter chooses which of
  your boxes may notify during that Focus.
- **Previews: When unlocked** (the default). A notification shows who did
  what as the title and, for replies and mentions, the first 120 characters
  of the message as the body; on a locked phone iOS hides the body. Markdown is flattened to plain text first: a table becomes
  `Header: value` lines, list items get a bullet, and headings, fences and
  link URLs are dropped.
- **Previews are per phone.** Set them to *Never* in the app's notification
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
    (`LIGUS_PUSH_NOTIFICATIONS = NO` in the app project's
    `Config/Shared.xcconfig`; set it to `YES` in your
    `Config/Local.xcconfig`). To get push you
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
| `topic not allowed on this relay` | the app's bundle id isn't one the relay pushes to | add it to `LANGSTROTH_RELAY_APNS_TOPICS` on the relay and restart it |
| `BadDeviceToken` | environment mismatch: a build run from Xcode has a sandbox token, an App Store or TestFlight build a production one | `LANGSTROTH_RELAY_APNS_ENV=auto` (the default) on the relay; open the app again |
| `DeviceTokenNotForTopic` / `TopicDisallowed` | the topic the relay used isn't the app on the phone | set `LANGSTROTH_RELAY_APNS_TOPIC` / `LANGSTROTH_RELAY_APNS_TOPICS` to the build's bundle id (the app reports it with its push token) |
| `InvalidProviderToken` | the relay's APNs key, Key ID or Team ID is wrong, or from another team than the app | fix the relay's `LANGSTROTH_RELAY_APNS_KEY*` / `TEAM_ID` |
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
  silence.
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
[Flow](flow.md), under **Review → Workspace**: the task board (open work and who claimed it), the review
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
define in `dashboard.toml` on the box. It is **Agents → Health** in the app,
shown when the dashboard isn't empty. Format and examples:
[`docs/dashboard.md`](https://github.com/langstroth-ai/ligus/blob/main/docs/dashboard.md).

## Accessibility

- **VoiceOver** says when an agent finishes or fails in the chat you have
  open, when an agent asks you a question, and when your vote is recorded,
  once each. A reply being written reads as "writing a reply" instead of
  being read token by token. A proposal's sections and each agent's question
  are headings for the rotor.
- **Larger text:** at accessibility sizes the chat keeps the transcript on
  screen; the notes above the composer fold into one line.
- **Increase Contrast** strengthens secondary text and lines; **Reduce
  Transparency** gives the top banner a solid background. Every status pairs
  a glyph with a word, so none depends on colour.

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

Everything except posting a message asks for Face ID (or Touch ID, or your
passcode) before the phone signs it, every time: the sign button says
**Approve with Face ID**, and the prompt names what you are approving.
Cancel, and nothing is signed or sent; the sheet stays so you can try again.
A phone with no passcode doesn't sign these. Admitting a box asks the same
way.

Chat messages don't ask for Face ID: a message you post, or a reply you type
in a notification, is signed without a prompt, behind the
[app lock](#app-lock-face-id) if you turned it on.

**If something changes while you confirm.** Confirming takes a few
seconds, and your box can write to the same chat meanwhile. Then nothing is
recorded, and the phone asks for a new draft. If the new draft shows
exactly what you confirmed, the phone asks once more on its own (the
passkey or Face ID again) with "Something changed while you were
confirming. Confirm again."; if it shows something else, the sheet shows it
and waits for your tap. A chat message never re-signs on its own: its sheet
comes back ("Something changed on your host while this was sending, so it
wasn't posted. Check it, then sign it again.") and waits for your tap, so
one tap never gives the box two signed copies. If it happens twice in a
row, the sheet says **Not recorded** with **Try again**.

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
- answering `@ligus`: only a desktop runs a chat's primary runtime;
- removing or re-roling members, and issuing invite links to people (the
  phone admits a box, nothing else about membership);
- direct messages: the phone's key isn't certified in a DM's own workspace;
- joining another workspace (a host serves one; DMs are the exception).

The full API the phone uses is in the [serve API reference](../../operate/reference/serve-api.md).

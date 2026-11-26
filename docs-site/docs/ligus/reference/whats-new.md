# What's new

Changes in each release, newest first, with links to where each is documented.
Full release notes are on the
[GitHub releases page](https://github.com/langstroth-ai/ligus/releases).

## Unreleased

### Fixed

- **Add an agent picks where it runs in the same step.** "Runs on" lists
  this device, your always-on boxes (`ligus serve` and workers) and your
  other computers, and saving writes the agent and its host together, so
  there's no separate step afterwards and only one passkey confirmation. For
  a model that goes through a model gateway, it starts on the one box that
  offers that gateway, and says so if you pick a host that doesn't.
- **A model gateway runtime is OpenAI-compatible.** Picking a model gateway
  in Add a runtime switches the kind to OpenAI-compatible and hides the
  Ollama-only settings (its address, keep-loaded and the context size sent
  to Ollama). Type the model the gateway serves.
- **First-run Ollama gets a real model.** Choosing Ollama during first
  setup offers the models installed on this computer (or a field to type
  one), instead of creating the runtime with `gpt-4o`. The first-run error
  note says "press Next", matching the button.

- **A reply written after you answer an agent's question reads after your
  answer.** An agent's reply is stamped with when it was posted (when its
  turn finished), not when the turn started, on the desktop and the phone. On
  the desktop, a question you already answered sits above the reply that
  reports your answer. Replies from devices still on 0.4.0 show right too,
  once the device or box showing them is updated.

## 0.4.0

Approve a box or a computer by comparing a short code, profiles and device
names, stricter device rules (rules 3), decision cards from agents
(Claude Code's own prompts included), proposal archiving, scratch
repositories, secrets out of the database and attachments that stay on the
relay while they're used. Upgrade every device, boxes and phones included.

### New

- **Decision cards from agents.** Agents ask with `ligus_ask_person` (the
  person who asked for the turn) or `ligus_ask_room` (anyone at or above a
  role, or a quorum who must agree), with 2 to 5 options or Yes/No and an
  optional reply. Only the people a card names can answer it, every answer
  is signed and bound to the exact card, and **Not now** on a room card is
  only your abstention. The card always says what your answer does: a
  question grants nothing. See
  [Questions to the chat](../addons/ligus-mcp.md#asks).
- **Permission requests are grant cards.** An ACP agent's request shows the
  exact command or file change, the agent's own description labelled as
  such, and Allow once and Deny. A passkey is asked in a lockdown, in dearth
  mode, and for a call outside the agent's sandbox.
- **Approve a box or a computer by comparing a code.** An Admin copies the
  workspace's approval address (`ligus admit address`, or workspace menu →
  **Admit a box…**); a box runs `ligus admit request --to ligusaddr1:…` and a
  computer `ligus devices request --to ligusaddr1:…`. The request reaches the
  Inbox and, for a box, a paired phone; both screens show the same 8-symbol
  code, and the approval takes one passkey. A headless box joins only once the
  approval is on the real workspace log, signed by an Admin's device. A
  mismatch grants nothing. On the phone, a push-started approval asks which of
  three codes the box shows. `ligus approvals list` and `approve` are the
  headless approver; `ligus admit address --reset` cuts off a leaked address.
  Paste and the fingerprint still work, and Kubernetes admissions are
  unchanged. See [Admitting a box](../../operate/ops/admitting-a-box.md#code-match)
  and [Add a computer](../features/linked-devices.md#code-match).

### Added

- **Scratch repositories.** No code yet? **New scratch repo**, the first
  choice in every repository picker (desktop, phone, lt, and the guided
  setup's code folder flow), has the host that runs the chat's agents create
  an empty repository for it. Agents work in it with the chat's branch,
  proposals, approvals and the sandbox as usual; a small **scratch** tag
  marks it. **Keep it as a repo** moves it out of the scratch folder, with an
  optional, confirmed **Publish to GitHub (private)**. Unkept ones are
  removed after 14 quiet days (a setting), with a notice two days before;
  their chats keep their history. On the command line: `ligus repo scratch`
  and `ligus repo keep`. See
  [Scratch repositories](../features/repositories.md#scratch).
- **Archiving proposals.** A settled proposal's row menu has **Archive**
  (for everyone, Contributor+) and every proposal has **Hide for me** (this
  device only). Settled proposals also leave the Review pane on their own
  after the workspace's auto-archive days (7 by default; an Admin sets it,
  or turns it off, in **Settings → Review**), a rule every device applies
  to the time the proposal settled, so nothing is written when one ages
  out. Nothing that still waits on someone is ever archived. **Archived
  proposals** (the Review pane's ⋯, or ⌘K) lists them across the
  workspace, with search and **Restore**. Archived and hidden proposals are
  out of the Inbox, the needs-you counts, the dock badge and agents'
  `ligus_read` (which takes `archived: true` to include them; the tool's
  schema changed for that). The phone gets an `archive` action and
  `GET /v1/proposals?status=archived`; `status=all` now leaves archived
  proposals out. See [Archiving proposals](../features/right-rail.md#archiving-proposals).
- **Your name and picture are one profile.** A rename or a new picture on
  the desktop shows for everyone in every workspace you're in, in past
  messages too; pictures no longer ride every message (16 KB at most, PNG,
  JPEG or WebP). `ligus profile set` does it headless. See
  [Settings → Profile](../features/settings.md#profile).
- **Device names sync.** **Rename…** in My devices, `ligus devices rename`,
  `lt --plain device-rename`; boxes name themselves from `--label`. A
  computer approved but never redeemed says so and offers **Remove**.
- **Claude Code's own permission prompts reach Ligus.** `ligus hook install`
  adds `ligus hook permission` as Claude Code's `PermissionRequest` hook, so a
  Claude Code session you run yourself shows its prompt as a grant card on
  the desktop and your phone. Allow once always needs your passkey (Face ID on
  the phone); no answer leaves the decision to Claude Code's own dialog. The
  desktop app answers the hook; a box and lt don't yet. `ligus asks list`,
  `answer` and `decline` answer questions at a terminal. See
  [Claude Code's permission prompts](../addons/claude-code-hook.md).
- **Linking a computer finishes on the computer.** When it redeems the code,
  the computer accepts your desktop's approval, names itself after its link
  request and, when it was already a separate member here, retires that
  member into you (only when that's safe; otherwise an Admin removes it).
  See [Add a computer](../features/linked-devices.md).
- **Rules v3.** A device certificate counts only when its device accepted
  it, duplicate members retire into their person, model keys are refused on
  the log. A new workspace moves to rules 3 with its founder's first
  passkey. On an older workspace an Owner switches in **Settings → Security
  → Stricter device rules**, with a list of devices whose approval would
  stop counting, and a passkey; there is no way back. Devices on Ligus 0.3
  are read-only on a rules 3 workspace. See
  [Rules v3](../../operate/security/index.md#rules-v3).
- **Look-alike names are tagged.** A name that looks like another
  account's, an agent's or Ligus shows as Name#abcd wherever a chat
  renders, prompts to agents included.
- **Attachments stay on the relay while they're used.** The sharer's devices
  and Admins' desktops renew a live attachment on the relay about every ten
  days, up to the relay's maximum age (a year by default). A file the relay
  no longer has says **This file expired on the relay**. A relay without
  renewals keeps files for its retention, as before. See
  [How long files stay on the relay](../features/attachments.md#how-long-files-stay-on-the-relay).
- **Secrets out of the database.** Invite and admission seeds live only in
  the OS secret store; old copies are moved there and removed from
  `ligus.db` once their invite or admission is closed. Sync progress is kept
  per relay and workspace. `ligus.db` records its layout version: a database
  written by a newer Ligus opens read-only (the CLI exits 3), and
  `ligus doctor` reports the version and lists database backups. See
  [Invite seeds, backups and the database version](../../operate/security/index.md#seeds-and-backups).
- **Phones state their person.** The phone app asks once to confirm *this
  phone is Ana's in Hive Lab*; that signed statement settles a phone
  another member also certified.

### Changed

- Every question card is plain text: no Markdown and no clickable links.
- Members on 0.3 can't answer the new cards until they update: their
  answers are kept but not counted.
- An open proposal that an earlier version dismissed shows in Review again:
  nothing that still waits on someone is archived. Reject it, or hide it
  for yourself.
- The relay's own member roster is deprecated for Ligus workspaces: every
  device enforces the signed roster.

### Upgrading

- **Upgrade every device**: desktops, `ligus` CLIs and lt, `ligus serve`
  and worker boxes, and phones. A 0.3 device is read-only on a rules 3
  workspace, and can't answer the new cards.
- **Relays.** Approval by code and attachment renewal need the new
  `langstroth-relay` release. On an older relay, `ligus admit request --to`
  and `ligus devices request --to` stop with exit 3 and name the paste
  command to use instead, and the desktop keeps the paste and fingerprint
  path; attachments expire at the relay's retention, as before.
- **Going back to 0.3** still opens the data (0.4 keeps the old tables
  written too), but invites issued by 0.4 must be issued again there. From
  0.4 on, a build that finds a newer database opens it read-only.

## 0.3.1

A security release: permission requests from agents, devices that act only
for their own account, and model keys that never sit in the workspace. Also
easier linking of a computer and a calmer relay claim. Upgrade every member.

### Security

- **"Allow" always means once.** Ligus picks only an ACP agent's one-time
  option. An agent that offers only a permission for the rest of its
  session gets it only when a person explicitly says yes to a question
  that says so; otherwise it is refused, even for a call your tier allows.
  See [ACP permissions](../addons/acp.md#permissions).
- **Only the host can ask for permission.** A permission request counts
  only from the device that runs the agent's turn. One that any other
  member or box posts shows as a plain question, attributed to them, and
  grants nothing; the host waits only on the request it posted.
- **Only the asker closes a question; only the right people grant.** A
  question closes only from the agent or its host. A permission request is
  granted only by the host's own person or the agent's owner, from one of
  their own devices. Other closes and answers stay in the chat and change
  nothing. See [Questions to the chat](../addons/ligus-mcp.md#asks).
- **Devices act only for the account they chose.** A device key is public,
  so any member could certify someone else's device for their own account.
  That could stop a `ligus serve` box, break a phone, block linking a
  computer, borrow a person on their private agents, inherit a removed
  member's history, or keep a removed member's device getting new keys.
  Now a device acts only for accounts it signed as itself and that
  certified it; a foreign certificate blocks and redirects nothing, and is
  shown quietly (`ligus devices list`, Add a computer). See
  [Add a computer](../features/linked-devices.md).
- **Model API keys are never stored in the workspace.** `ligus add-runtime
  --secret` is gone (it's a usage error): give a workspace runtime a key with
  `--secret-ref <NAME>` and set `LIGUS_WS_SECRET_<NAME>` on each worker. A
  key an older version stored in the workspace is never used, shown or
  exported; such a runtime reads as "stored key (unused)", runs only if it
  can run keyless, and `ligus doctor` warns about it (`runtimes.inline-key`).
  Every member's device received that key, so rotate it with the provider and
  move the runtime to `--secret-ref`. See
  [Headless agents](../../operate/concepts/headless-agents.md).

### Changed

- **Linking a computer without retyping codes.** On the desktop, Add a
  computer's last step has one main button, **Copy for lt**, which copies
  the grant alone, and shows This desktop's fingerprint large with its own
  copy button. On the computer, paste it in lt, or run `ligus devices
  redeem` (or `lt --plain link-redeem`) with nothing after it: it reads a
  hidden paste or the clipboard and asks *Is that your desktop? [y/N]*.
  `ligus devices request` prints only the request on stdout (`| pbcopy`
  copies exactly it), with `--copy` and `--json`. A link waiting for the
  desktop's code shows in `ligus status`, and expired requests are cleaned
  up. See [Add a computer](../features/linked-devices.md#how-it-works).

- **Linking a computer is easier to find.** ⌘K **Add a computer to my
  devices…** (type link, computer, device, lt or terminal) and ⌘K **My
  devices**; Profile has a **My devices** link at the top. In People, a
  member that looks like you on another computer says so, and its row menu
  opens Add a computer for it, with removing that member offered once it's
  added. In lt, Settings and People say when the terminal is its own member,
  and **D** links it. See
  [Already invited yourself?](../features/linked-devices.md#already-invited-yourself).
- **Revoked devices are out of the way.** My devices folds them into a
  collapsed **Revoked (N)** group, one line each, with the cleanup report
  behind **Details** and **Hide from this list** (it stays revoked). One whose
  cleanup isn't finished stays in view with **Retry now**. See
  [Revoked devices](../features/linked-devices.md#revoked-devices).

### Fixed

- **Claiming a workspace on the relay.** The People pane offered
  **Claim ownership** to every device, and a device whose relay token names
  no account got "the relay refused (HTTP 401)". It now offers the claim
  only when this device's token names an account; on a self-hosted relay
  without one it offers **Add a relay token**, and on the community relay
  (no sign-in) or a relay with membership off it offers nothing. The
  warning is one calm line, with what the relay checks and what Ligus
  enforces anyway behind **?**, a claim asks first, and a refused claim
  says what's missing and how to fix it. A workspace that is claimed but
  hides its roster from you no longer reads as "anyone can write". See
  [Claiming a workspace](../../operate/ops/server-side-membership.md#claiming-a-workspace).
- **Refusals say why.** A device signing as someone who is no longer a
  member (for example an lt left open while the computer was linked to
  your devices) is told to link the computer again or ask for an invite,
  and a role that's too low is told which role it needs, instead of "role
  Viewer cannot perform messageAppended".

### Upgrading

- **Upgrade every member.** The permission-request rules apply on each
  member's own device: a 0.3.0 device still shows a forged permission
  request as a real one and honours someone else's close (hosts already
  check who granted a permission, so it can't grant one by mistake). 0.3.0
  and 0.3.1 devices sync normally.
- **Rotate a model key stored in the workspace.** If `ligus runtimes`
  shows "stored key (unused)", rotate that key with the provider and set it
  on each worker with `--secret-ref`.

## 0.3.0

A calmer desktop, guided setup for everything you connect (on the desktop,
the CLI and lt), adding a second computer to your own devices, custom
themes, and security fixes for agent tools.

### New

- **A calmer desktop.** List rows are one line: who or what, then one
  muted status. A row's actions are in its menu (right-click the row, or
  click **⋯**), each pane shows one main action, and the longer
  explanations sit behind **?** instead of under every pane. Settings
  follows the same pattern. See [Settings](../features/settings.md).
- **Guided setup on the command line.** `ligus setup` lists seven flows
  and what each has done on this device: set up an agent, connect a tool
  (an MCP server), add a code folder, add a box, add a phone, invite a
  person and set up a relay. `ligus setup <flow>` runs one: it checks first,
  writes only when every check passed, can be run again safely, and stops
  with exit 4 at a step only a person can do (approving a box with a
  passkey, pairing a phone, creating an invite). Every prompt is a flag,
  `--json` prints one `ligus.checks.v1` report and `--dry-run` writes
  nothing. Secrets are read only from stdin or an owner-only file. The
  earlier `ligus setup [--apply]` is now `ligus setup runtimes [--apply]`.
  See [Set up](../getting-started/set-up.md) and `ligus help setup`.
- **Guided setup in the desktop.** ⌘K lists **Set up an agent**,
  **Connect a tool**, **Add a code folder**, **Add a box**, **Add a
  phone**, **Invite a person** and **Set up a relay**; `/setup` in the
  composer does the same. Each opens one wizard, one step per screen, that
  checks before it changes anything and shows a failure's fix right under
  it. An empty list of runtimes, MCP servers or hosts offers its flow.
- **Guided setup in lt.** `u` (or the palette) lists the same seven
  flows, each a popup one step per screen, and `lt setup <flow>` runs one
  from a script with the same flags, `--json` and exit codes.
- **Choose which agents may use a tool.** A tool (MCP server) can be
  limited to some agents. The choice is kept on the device that runs the
  server, never in a file a commit could change, and a turn of the
  default agent gets only tools every agent may use.
- **Add a computer to your devices.** Use Ligus on a second computer (`lt`,
  or the `ligus` CLI) as yourself, not as a second member with your name:
  **Settings → Profile → My devices → Add a computer** on the desktop, one
  passkey confirmation, and a grant sealed to that computer. `lt` has **Add
  to my devices** on first run and **Link this computer to my devices** in
  the palette; the CLI has `ligus devices request`, `redeem`, `approve` and
  `list` (`ligus help devices`). Already invited yourself? Linking offers to
  remove the duplicate member; their messages keep their name. See
  [Add a computer](../features/linked-devices.md).
- **Custom themes.** Settings → Appearance → **Import a theme…** reads a
  theme file (TOML or JSON) that sets colours for light, dark or both, on
  top of one of the six palettes; **Export current theme** saves the theme
  in use as a starting file. The same file works in lt. Colours only, each
  checked strictly; hard-to-read pairs are flagged, not refused. Imported
  themes stay on this device. See [Themes](../features/themes.md).

### Fixes

- **@hive still reaches the default agent.** Messages, scripts and older
  phones that mention `@hive` are answered by the default agent again, and
  its replies from the Hive builds show as Ligus. Nothing stored is
  rewritten. New agents can't be named `ligus`, `hive`, `primary` or a
  look-alike of them, on the desktop or with `ligus add-agent`.
- **My devices no longer points at `ligus serve enroll`.** That command was
  removed in 0.1; a box that still names you as its owner now shows how to
  admit it (`ligus admit request`, then workspace menu → Admit a box).
- **`ligus app` on macOS opens the installed app.** It asked Launch
  Services by bundle id, which picks the highest `CFBundleVersion` it has
  seen, so an old test build on a mounted disk image could open instead of
  `/Applications/Ligus.app`. It now looks in a fixed order (`LIGUS_APP`,
  `/Applications`, `~/Applications`, then Spotlight, never a disk image or
  an App Translocation copy unless `LIGUS_APP` names it) and opens that
  bundle by path. It notes on stderr when the app and the CLI are different
  versions, says so when Ligus is already running from another path
  instead of switching to it, and `--json` reports the path and version.
  See [Opening the desktop app](cli.md#opening-the-desktop-app).
- **The keychain stops asking (macOS).** A release of Ligus that finds its
  keychain items saved by a different Ligus build (a test or development
  build) no longer prompts again and again. It reads the keychain's unlock
  password once per launch instead of on every use, and after you allow it
  once it re-creates the unlock item, and the items inside the Ligus
  keychain, so they trust the release; nothing asks again. The `ligus` CLI
  and development builds never rewrite an item's access.
- **Clearer keychain errors.** When macOS doesn't let Ligus read its
  unlock password (you chose Deny or Cancel, or there's no desktop session
  to ask in, as over SSH), Ligus says so and what to do (choose **Always
  Allow**), instead of reporting the password as missing. The desktop
  shows this with a **Try again** button. `ligus doctor` reports it as
  `secrets.keychain-access`, without prompting. See
  [The Ligus keychain on macOS](../../operate/security/runbooks.md#mac-keychain).

### Security

- **Tool restrictions apply to Claude Code turns.** A tool limited to some
  agents is left out of the tools Claude Code is given for any other agent.
  If the file that holds those limits can't be read, no tools are offered.
- **Tokens go only to their own server.** A saved relay access token is
  sent only to the relay it was saved for, and a tool's token only to the
  server it was saved for. Relay and tool URLs must use https (plain http
  only for this machine) and can't carry a user name or token, and tool
  errors no longer show the URL.

### Upgrading

- **`ligus setup --apply` is now `ligus setup runtimes --apply`.** The old
  spelling still works in 0.3.0 and is removed in the next release; update
  scripts that use it.

## 0.2.1

A security release: a chat's history is now kept per workspace, so a
member of another workspace you share can't hide, re-home or disturb your
chats. Upgrade every member of a workspace.

### Security

- **A chat's history belongs to its workspace.** Chat ids aren't
  namespaced by workspace, so someone in another workspace you share (an
  ex-member, a DM peer) who knew a chat's id could sign events under it
  and, on your device, decide which workspace the chat belonged to, blank
  it, or make your next message unreadable on other devices. Each chat's
  log is now kept per workspace; their events form a separate chat in
  their own workspace, visible only there. This fixes the 0.2.0 known
  issue.
- **Event ids are unique per workspace.** An event id planted first in
  another workspace no longer keeps the genuine event out of its chat.
- **Checks use the chat's own workspace.** The check a box runs before it
  applies code and the agent access gates read the chat in its own
  workspace. See [Agent access](../features/agent-access.md).
- **Chat lists and notifications carry the workspace.** The chat list,
  the Inbox and notifications know each chat's workspace, and the command
  palette opens a chat in it. Deleting a chat removes only that
  workspace's copy.
- **Secret redaction fails closed.** If the fast matcher can't be built,
  every registered secret is still masked, one at a time, instead of none.
- **Upgrade every member.** Each device protects its own copy of a chat;
  upgrade every desktop and `ligus serve` box in the workspace.

### Upgrading

- **A one-time database upgrade.** The first launch of 0.2.1 (the desktop,
  `ligus serve` or a worker) upgrades the local database once. It can take
  a few seconds on a large history and needs about 2.5 times the
  database's size in free disk space. If there isn't enough, Ligus says
  how much to free ("free N GB ...") and changes nothing. A temporary
  backup is kept beside the database and removed after the next clean
  launch.

## 0.2.0

A calmer desktop (an Inbox for what needs you, a quiet status bar, focus
mode and quieter notifications), a model picker in every chat, the system
passkey sheet on macOS, one set of CLI conventions, a more reliable event
store, and security fixes. Upgrade every member of a workspace.

### Desktop

- **An Inbox for everything that needs you.** One list, most urgent first:
  an agent's question (answer it right there), proposals waiting on your
  vote or ready to apply, a failed run, changed sandbox grants, invites and
  device requests. Things that only report what happened stay out of it.
  See [Inbox](../features/inbox.md).
- **Badges count only what needs you.** The Inbox, the dock icon and the
  tray show the same number; unread messages keep their dot in the sidebar.
- **A quiet status bar, and one banner at most.** Sync, agents working,
  queued work, errors and an available update sit at the foot of the
  window. Only a lockdown or dearth mode gets a banner across the top, one
  at a time. See
  [The status bar and the one banner](../features/inbox.md#the-status-bar-and-the-one-banner).
- **Focus mode.** **⌘.** (or "Focus mode" in the ⌘K palette) hides
  everything but the chat and the message box; press it again to bring
  everything back. See [Settings](../features/settings.md).
- **Quiet notifications.** The desktop notifies you only about mentions,
  an agent's question, runs you asked for, your vote and agents of yours
  that someone else used. Nothing shows while the window is in front, and
  a burst becomes one notification ("3 chats finished"). See
  [Desktop notifications](../features/mentions-and-notifications.md#desktop-notifications).
- **Density.** **Settings → Appearance → Density**: Compact, Comfortable
  (the default) or Spacious. Text is quieter without fading, and each
  heading level has one style. See [Appearance](../features/settings.md#appearance).
- **Tooltips and menus.** Themed tooltips appear on hover and on keyboard
  focus, with the shortcut as a chip, and menus work from the keyboard.
- **Chat rows show their state** (Working, Queued, Needs you, Done,
  Failed) and where the chat runs; the command palette is better and
  **⌘/** opens a shortcuts sheet. Destructive actions ask more clearly.
- **One colour source** for the desktop, the phone app and `lt`, checked
  for contrast in CI.
- **Implement is hidden on a rejected proposal**, even when it still has
  its quorum of approvals.
- **Pick the model while you chat.** The route pill under the composer
  opens a second step for each runtime: **Default**, the models its harness
  offers (Claude Code, Codex, Copilot CLI, Antigravity, Ollama and
  OpenAI-compatible servers), and **Other…**. The choice is kept per chat
  on this computer and shows in the line under the chat title, and each
  reply is labelled with the model it ran with. A model your account can't
  use is reported in one line with **Pick another model** instead of a
  failed reply. Settings offers the same lists. See
  [Picking the model for a chat](../getting-started/configuring-a-runtime.md#pick-a-model).

### Passkeys

- **The system passkey sheet on macOS.** For workspaces that use the
  `ligus.ai` passkey relying party (the default), the desktop asks with
  macOS's own passkey dialog (Touch ID, iCloud Keychain, an iPhone nearby)
  instead of opening your browser. The browser stays available from the
  sheet. See [Native passkeys](../../operate/security/native-passkeys.md).

### CLI

- **`ligus doctor` runs every check** and prints one `ligus.checks.v1`
  report (`--json` for the machine form), shared with the deploy scripts.
  See [CLI conventions](../../operate/reference/cli-conventions.md#checks-v1).
- **One exit-code table, 0 to 7**, for `ligus` and the deploy scripts:
  done, failed, usage, a fixable precondition, a person must act, not
  found, refused by workspace rules, offline. `--legacy-exit-codes` keeps
  the old codes for this one release. See
  [Exit codes](../../operate/reference/cli-conventions.md#exit-codes).
- **Errors say what, why and how to fix it**, and with `--json` an error is
  one `ligus.error.v1` object. See
  [Errors](../../operate/reference/cli-conventions.md#errors).
- **Common flags everywhere:** `--flag=value`, `-o`, `-q`, `--color`,
  `--dry-run` (a command that can't preview refuses it instead of running)
  and `ligus send --wait`. Colour only on a terminal. See
  [Common flags](../../operate/reference/cli-conventions.md#flags).
- **One help layout** across `ligus` and the deploy scripts, and
  `ligus help --json` for agents and completion scripts. zsh and fish
  completions for `ligus-local` and `ligus-k8s`. See the
  [CLI reference](cli.md).

### Reliability

- **No lost writes when the phone, the desktop and sync write at once.**
  A message or vote written while sync was storing events in the same chat
  could be refused ("UNIQUE constraint failed"), and a box's sync could
  fail with "database is locked", most often on Windows. Every write now
  takes the database's write lock up front and waits its turn, and a
  passkey or phone-signed action that raced another write asks again
  instead of failing.
- **Safer cleanup.** Purging a workspace keeps one that gained an event
  after its backup was taken, and a second cleanup in the same second no
  longer deletes the first one's backup.
- **Faster secret redaction.** Registering many secrets no longer stalls
  replies; the CLI's test suite runs about seven times faster.

### Security

- **A proposal comment is by the member who signs it.** Any member could
  post a comment on a proposal that showed as someone else's (the Owner's,
  for example), and **Send to agent to revise** forwarded it to the agent as
  part of the change request. Now a comment whose named author isn't its
  signer is refused, its role is the signer's real role, only verified
  comments are forwarded to an agent, and an edit to a proposal can no
  longer replace or rewrite its comment thread.
- **Upgrade every member.** The check runs on each member's own device. In a
  workspace that still has 0.1.x members, those older clients can still show
  a forged comment under someone else's name and forward it to an agent when
  their user asks for a revision. Upgrade every member's desktop (and any
  `ligus serve` box) to this release.
- **The phone sees its own vote.** A box showed a phone the box's own role
  and vote on a proposal instead of the phone owner's.
- **Avatar colours are checked.** A synced avatar colour that isn't a
  colour is ignored, and security alerts rank first.
- **`ligus doctor`'s relay probe** has a timeout and redacts what it
  prints; durations are capped, and `--dry-run` on a command that can't
  preview is refused.
- **Model listing runs only the harness's own binary**, asks each endpoint
  separately and refuses model ids that start with a dash. The deploy
  scripts never echo an unknown argument's value.
- **url 2.5.9**, and sign-in links open only `http` and `https`.

### Known issue

- A member of another workspace you share who knows a chat's id can disturb
  that chat on one device. Fixed in 0.2.1.

## 0.1.1

A patch release: add your phone by scanning the desktop's code, Face ID
before every phone action that counts, a faster desktop launch, clearer
buttons and proposals, and security and membership fixes.

### Phone

- **Add a phone by scanning the desktop's code.** On the desktop,
  **Settings → Profile → My devices → Add a phone → Show code**; on the
  phone, **Settings → This phone → Add to my devices** and scan. The phone's
  request reaches the desktop through your `ligus serve` box; you compare
  the fingerprint and approve with your passkey, and the phone pairs. A
  brand-new phone that isn't paired yet works too: through the relay the box
  gives it a provisional connection that reaches only the add-a-phone
  routes. Pasting the phone's code stays as **Can't scan?**. The QR's
  one-time secret keeps the box and the relay from swapping in another key,
  and a 5-second hold and a tripwire alert catch a second phone answering the
  same code. See [Add your phone](../features/phone-app.md#add-this-phone),
  [Adding a phone by QR code](../concepts/security.md#add-phone-qr) and the
  [`/v1/phone-enroll/*` routes](../../operate/reference/serve-api.md#phone-enroll).
- **Clearer pairing errors.** When the relay refuses the box's control
  connection, `ligus serve` now logs the relay's reason (for example
  `rate_limited: too many attempts`) instead of only "expected challenge".
  A `4029` waits at least a minute and says that the relay counts failed
  attempts per IP address (per /64 on IPv6), so a phone retrying a used-up
  pairing link from the same network counts too. After **Add this phone**
  and `ligus phones certify`, the desktop and the CLI say what to do next
  (let the box sync, tap Retry on the phone, or scan a new `ligus pair`
  link), and the panel marks the phone's name as the phone's own claim. See
  [Troubleshooting](../help/troubleshooting.md#the-phone-cant-connect).
- **My devices remembers the phone's name.** The desktop that added a phone
  (by QR code or with **Can't scan?**) lists it under the name the phone gave
  itself instead of "Device" and the start of its id. The name is kept only
  on that desktop; your other desktops still show "Device xxxxxxxx". See
  [Add your phone](../features/phone-app.md#add-this-phone).
- **One My devices row per phone.** Re-adding a phone you already hold
  updates its name instead of leaving an old row next to the new one, and
  each phone says which box it's paired with and when it was last seen
  ("Paired with home-server · seen 2 min ago"). Revoking a phone names it in
  the prompt and says that it can't come back with its current key: on the
  phone, **Reset this phone's key**, then **Add to my devices**. See
  [Revoking a phone](../features/phone-app.md#revoked-phone).
- **The phone shows the workspace's name**, not the box's label. An Owner's
  or Admin's desktop writes the name to the workspace once, when it opens
  the workspace. See
  [The workspace's name](../features/phone-app.md#workspace-name).
- **Face ID before every phone action that counts.** Votes, answers to an
  agent's questions, apply requests and withdrawals, lockdown clears and box
  admissions ask for Face ID (or Touch ID, or your passcode) every time,
  and the prompt names what you are approving. Cancel, and nothing is
  signed. Chat messages, from the composer or a notification reply, don't
  ask. See [Phone-signed actions](../features/phone-app.md#phone-signed-actions).
- **Vote on an offered file from the phone.** A proposal that offers a file
  now carries the file's reference, so the phone shows its name, type, size
  and hash and ties your vote to them instead of refusing to vote. See the
  [serve API reference](../../operate/reference/serve-api.md).
- **Resetting the phone's key ends its pairings.** **Reset this phone's
  key** also replaces the keys the phone paired with, so pair it again with
  a new `ligus pair` link. See
  [Revoking a phone](../features/phone-app.md#revoked-phone).

### Security

- **A box removed from the relay roster loses access to new traffic.**
  Removing a box under **People → Team members (server-enforced)** used to take it off
  the relay only: its service member stayed on the workspace roster, so the
  key rotation that follows sealed the new key to it and it kept reading new
  messages. Now the desktop that bootstrapped the box (**People → Remote
  agents**) removes the box's member from the workspace first (or closes its
  admission if it never joined), then rotates, so the box is left out. The
  bootstrap command now carries the box's admission, so generating it asks
  for your passkey. A relay entry the desktop can't match to a workspace
  member says so instead of reporting success. See
  [Remove a box](../../operate/security/runbooks.md#remove-box).
- **Relay refusals stop membership changes.** Removing a member, changing a
  role or revoking an invite used to carry on (editing the roster and
  rotating the key) when the relay refused, for example because you aren't
  an Admin or would remove the last Owner. Now the desktop stops and shows
  the relay's reason. New boxes get a relay identity (`ligus-box:<id>`) that
  can't be mistaken for a person's account.
- **Only an Admin names the workspace.** A Contributor or a Viewer can't set
  or take over the name the phone shows.
- **My devices believes only your own boxes.** A phone's name and "paired
  with" line come only from boxes signed by your own devices. Those reports
  are dated, tied to the workspace and limited in size, and a box reports
  only your phones, not a teammate's.
- **Adding a phone by QR code: tighter checks.** The box takes a desktop's
  answer only for requests made to that desktop; every phone that answers a
  code is visible at once, so the 5-second hold catches a second phone; and
  the desktop looks once more right before it approves, refusing with
  "Two phones answered this code" if another phone is there. See
  ["Another phone answered this code"](../features/phone-app.md#another-phone).

### Desktop

- **Faster launch.** The desktop no longer walks your whole workspace folder
  before its window opens; a folder of about 500,000 files had held the
  launch for about 12 seconds. Switching workspaces is quicker for the same
  reason, and the agent sandbox check runs in the background.
  `LIGUS_LOG=startup=debug` logs how long each startup step takes.
- **Buttons look and feel like buttons.** Buttons, links, menu items and
  tabs show a pointer and a gentle hover and press, with a clear focus
  ring; disabled ones show that they are.
- **Markdown in proposals.** Proposal bodies, workflow proposal
  descriptions, refinement notes, alternatives, rationales and agents'
  questions render Markdown, with the same link and image safety as chat.
- **Your own vote shows.** Once you've voted, the proposal card says
  "You approved" or "You rejected" with a link to change it. When the
  quorum is met, Implement is the main action.
- **Copying a code block leaves off the final newline**, so pasting a
  command into a terminal no longer runs it at once.
- **Settings sections fail on their own.** A Settings section that hits an
  error shows a small card with **Try again** instead of replacing the whole
  app.
- **Admit a box from the workspace menu.** **Admit a box…** is in the
  workspace menu (the workspace name at the top of the sidebar) and the
  command palette, not only on the rail's right-click menu. See
  [Admitting a box](../../operate/ops/admitting-a-box.md).
- **The command palette reaches every pane and every Settings section.**
- **Warnings have their own colour.** An amber-orange, apart from the honey
  accent, in light and dark.
- **Readability fixes.** **Save access**, the active Settings button and the
  Create/Join tabs now meet contrast, and menus opened from the sidebar are
  readable again.
- **The tour describes the Diff tab correctly.** It shows the chat checkout's
  uncommitted changes; proposals are in the **Review** pane.

### CLI

- **The CLI documents itself.** `ligus help` lists every command and a set
  of task guides (`ligus help box`, `phone`, `remote`, `worker`,
  `kubernetes`, `repos`, `agents`, `security`, `env`, `troubleshooting`, and
  `agents-guide` for coding agents). `ligus <command> --help` prints the
  command's purpose, flags with defaults, environment variables, examples and
  exit codes. Help prints to stdout and exits 0 (it was stderr and 2), and
  never opens the data dir; a usage error now exits 2. New: `ligus status`
  summarises the data dir, workspace, relay, admission and serve state, and
  `--json` works on `status`, `whoami`, `chats`, `runtimes`, `agents`,
  `hosts`, `repos list`, `remote status` and `phones list`. See the
  [CLI reference](cli.md#help).
- **`ligus app`** opens the desktop app and returns at once, detached from
  the terminal. See the [CLI reference](cli.md).
- **`ligus-local verify` finds the systemd unit by itself.** On Linux, when
  `ligus-local start --systemd` installed the user unit for that data dir,
  `verify` (and `status`) checks it with `systemctl --user is-active`
  without `--systemd`, as it already finds a LaunchAgent on macOS. See the
  [CLI reference](cli.md).

### Docs

- **Who can use your agents.** A new page on agent access: Only me or
  everyone in the workspace, daily turn limits and the spending cap, the
  refusal replies, notify-on-use and usage. See
  [Who can use your agents](../features/agent-access.md).
- **Fixes.** UI labels (**Workspace → Agent roster**, **Settings → Profile →
  Privacy**), and what is actually published for Kubernetes: the relay and
  langstroth-auth charts and the relay image pull anonymously from
  `ghcr.io/langstroth-ai`; the ligus chart and image aren't published. See
  [Kubernetes](../../operate/deploy/kubernetes.md).
- **Revoking your own device needs no Admin.** The runbook said it did; any
  member can revoke a device their own account holds. See
  [Revoke one of your devices](../../operate/security/runbooks.md#revoke-device).

### Release

- **Homebrew tap updates no longer race.** The formula and the cask are
  pushed to the tap one after the other, and a rejected push rebases and
  retries, so a release no longer leaves the tap half updated.
- **The Homebrew cask says the app updates itself** (`auto_updates true`).
  `brew upgrade` leaves Ligus to its own updater instead of offering an
  upgrade it already did; `brew upgrade --cask --greedy ligus-app` still
  moves the cask on.

## 0.1.0

The first release of Ligus (short for *ligustica*): the desktop app, the
phone app and the `ligus` CLI, with the Langstroth platform behind them (the
relay, langstroth-auth and the Helm charts). The CLI is `ligus`, settings are
`LIGUS_*` environment variables, workspace configuration is
`ligus.config.toml` and per-repository state lives in `.ligus/`.

### Workspaces and sync

- **Local-first, end-to-end-encrypted workspaces.** Each device keeps its own
  copy of a workspace as a signed event log. Devices sync through a
  content-blind relay, or directly peer to peer, and the relay only ever
  stores ciphertext. See [Workspaces & chats](../concepts/workspaces.md).
- **A community relay to start with.** Onboarding, **Add workspace → Create**
  (when no relay is set) and **Settings → Team sync** offer
  `relay.langstroth.ai`, a free, best-effort relay, next to your own relay
  and no relay. It is used only when you pick it. See
  [The community relay](../../operate/networking/community-relay.md) and
  [Self-hosting a relay](../../operate/networking/self-host.md).
- **Every device judges history the same way.** One authorization engine
  judges each event at its position on the workspace config log: votes count
  at the voter's role when cast, for the exact content they name; later
  promotions and demotions never rewrite earlier verdicts. Every author must
  be a member at the event's position, and joining needs an Admin's
  single-use invite. See [Authorization](../concepts/security.md#authz-at-position).
- **Events that can't be judged yet never stop sync.** An event signed by a
  device whose certificate or admission hasn't arrived waits aside on the
  device (bounded, counted under Settings → Team sync and in `ligus doctor`)
  and applies once it does. A key someone else registers under a member's
  account is refused and attributed to whoever signed it. See
  [held events](../../operate/security/threat-model.md#held-events) and
  [account keys](../../operate/security/threat-model.md#account-keys).
- **Only the person you invite can use the invite.** An invite (to a
  workspace or a DM) carries a redeem key whose secret is only in the code,
  so another member who sees the invite on the workspace log can't redeem it
  in that person's place. See [Invites & joining](../features/invites.md).
- **A DM invite waits for their device.** Opening a DM with someone whose
  contact id your device doesn't know yet holds the invite, sealed to your
  device's key, and delivers it once their contact id arrives (for up to 7
  days), with the link as a fallback. See
  [People, contacts & DMs](../features/collaborators.md#when-their-device-isnt-reachable-yet).
- **Passkeys for admin changes.** Membership and role changes, device
  revocation, key rotation, guards, zones, workflow definitions, agent hosts
  and human-gated approvals need a passkey (Touch ID, Windows Hello, a phone,
  a security key), verified on every device. Register passkeys and a recovery
  key when you create a workspace (or in **Settings → Passkeys & keys**). See
  [Passkey presence](../concepts/security.md#passkeys).
- **Signed key rotations on every host.** Rotating the workspace key is a
  signed event on the workspace log, sealed to keys each device published
  itself; `ligus serve`, `ligus worker` and the CLI follow rotations too.
- **Secrets in the OS secret store.** Keys and tokens live in a dedicated
  macOS keychain, the Secret Service or `systemd-creds`. See
  [Key storage](../concepts/identity.md#key-storage).
- **Organization sign-in.** `ligus auth login` and Settings → Team sync sign
  in through [langstroth-auth](../../operate/deploy/ligus-auth.md) with your
  identity provider (setups for [Pocket ID](../../operate/deploy/ligus-auth-pocket-id.md)
  and [Dex](../../operate/deploy/ligus-auth-dex.md)) and keep a short-lived relay
  token fresh.

### Agents and runtimes

- **Bring your own runtime.** Hosted APIs (Anthropic, OpenAI, OpenRouter,
  Gemini), local models (Ollama, MLX and any OpenAI-compatible server) and
  CLI agents (Claude Code, Codex, Copilot CLI, Antigravity, aider, pi). See
  [Configuring a runtime](../getting-started/configuring-a-runtime.md),
  [Offline setup with local models](../getting-started/offline-local-models.md)
  and the [runtime matrix](runtime-matrix.md), which is run before every
  release.
- **Subscription CLIs over ACP.** Copilot CLI, Antigravity and Codex (and
  Claude Code with an API key) run over the
  [Agent Client Protocol](../addons/acp.md); their permission requests follow
  your tier or come to you as a question. Headless adapters are the fallback.
- **Typed tools for agents.** Agents file proposals, hand off, leave marks,
  read the workspace and ask you questions through the
  [Ligus tools](../addons/ligus-mcp.md). A question pauses the agent's turn
  until someone answers, from the desktop or the phone. Agents sign with
  their own keys and can only propose.
- **One agent-turn engine on every host.** The desktop, `ligus worker`,
  `ligus serve` and the [`lt` terminal app](https://github.com/langstroth-ai/ligus-term)
  run agent turns through the same engine, with the same repository gate and
  sandbox policy. See
  [One turn engine on every host](../concepts/agents.md#one-turn-engine-on-every-host).
- **You decide who can use your agents.** A personal agent is "Only me"
  until its owner opens it to everyone; a shared agent is everyone's unless
  an Admin restricts it. Every person behind a turn must be allowed, through
  hand-offs, cascades and workflows too, and the offline fallback never runs
  someone else's agent on the sender's device. Owners set daily budgets
  (turns per member, an estimated-spend cap), see per-member usage, can be
  notified when someone else uses an agent, and keep their plan window for
  their own requests. `@ligus` in a shared chat answers only the person
  whose device runs it, unless they open it in Settings. The `@mention` autocomplete greys out agents you can't
  use, and a model gateway can count per member through the pseudonymous
  `X-Ligus-Member` header. See
  [Who can use an agent](../concepts/agents.md#who-can-use-an-agent).
- **One answer per turn, from a device that has the code.** When several
  devices could answer a turn, they agree through signed claims; a host with
  a checkout of the chat's repository goes first, and a holder that goes
  quiet is replaced. See
  [Which device answers](../features/repositories.md#which-device-answers).
- **Multi-agent chats, workflows and proposal sets.**
  [Multi-agent collaboration](../features/multi-agent.md),
  [agentic workflows](../features/workflows.md) that agents author as
  reviewable proposals (the Workflows pane lists and runs them),
  [proposal sets](../features/proposal-sets.md) for weighing alternatives,
  [Flow](../features/flow.md) for hand-offs, routing and the task board, and
  [scheduled agents](../features/scheduling.md).
- **Workspace memory and task state.** Shared notes agents and people read
  and propose changes to, and live task state per chat. See
  [Workspace memory](../features/memory.md).
- **Review shows exactly what Implement does.** Each proposal shows its kind,
  author and signature, config impact and gates; Implement previews the exact
  command or instruction, agent, runtime, sandbox and network.

### Code

- **Workspace repositories.** A workspace names the repositories it works on;
  each device keeps its own checkouts, and hosts report which ones they have
  (never where). A chat picks its repository; chat branches start from a
  pinned base, applied proposals replay on every host, and an apply never
  half-applies. Shared repository and host aliases are names only. See
  [Workspace repositories](../features/repositories.md) and
  [Git integration](../features/git.md).
- **Code hosts.** GitHub (and GitHub Enterprise Server), GitLab, Forgejo,
  Gitea and Codeberg, with draft pull or merge requests opened from a chat
  branch, and fork-aware pull requests opened upstream. See
  [Code hosts](../features/code-hosts.md) and
  [Linked accounts](../features/linked-identities.md).
- **Sharing files.** Attachments in chats, checked before you save them (and
  run through your own scanner if you set one up). See
  [Sharing files](../features/attachments.md).

### Security

- **Agent processes run in an OS sandbox.** Seatbelt on macOS; bubblewrap +
  Landlock + seccomp on Linux, or a Landlock + seccomp tier where bubblewrap
  can't create a user namespace. No reads of Ligus's data dir or credential
  dirs, writes only in the worktree, and the network only through a per-turn
  egress proxy. A host without a sandbox runs no local agent turns. See
  [The OS sandbox](../concepts/security.md#os-sandbox) and
  [The Linux agent sandbox](../../operate/ops/linux-sandbox.md).
- **Your home dir is off limits to agents; the shell is the Owner's.** Every
  agent process runs with a home of its own for the turn. An Admin grants
  extra paths in **Settings → Sandbox** with a passkey, and a shell is used
  only in turns the workspace Owner starts unless an Owner grants it.
  `ligus doctor --sandbox` shows the effective policy. See
  [Your home dir is default-deny](../concepts/security.md#home-default-deny),
  [Grants on this device](../concepts/security.md#grants-on-this-device) and
  [The shell is the Owner's](../concepts/security.md#owner-only-shell).
- **One author gate on every host.** A turn runs at the role of the humans
  who started it: a write-capable turn needs an Admin's message and a
  read-only one a Contributor's; agent replies with no human behind them run
  read-only. See [The author gate](../concepts/security.md#author-gate).
- **Claude Code turns get only what the turn needs.** Ligus starts Claude
  Code without your own settings, hooks, plugins, skills and MCP servers, and
  without cloud routines or background features; turn your own setup back on
  per workspace in **Settings → Guards → Your own agent setup**. See
  [What a Claude Code turn can and can't do](../concepts/security.md#claude-code-turn).
- **Egress presets.** Opt-in sets of hosts for a chat's egress allow-list
  (`package-registries`, `claude-connectors`). See
  [Egress presets](../concepts/security.md#egress-presets).
- **Trust this workspace.** Runtimes and MCP servers a repository supplies
  stay inert until you enable them on your device (`ligus trust`). See
  [Trust this workspace](../concepts/security.md#workspace-trust).
- **Workspace guards.** Settings → Guards collects
  [dearth mode](../features/guards.md#dearth-mode),
  [chat lockdown](../features/guards.md#chat-lockdown),
  [plan-window throttles](../features/guards.md#plan-windows-staggered-throttles)
  and [hygiene](../features/guards.md#hygiene), on top of
  [one sandbox envelope for every runtime](../concepts/security.md#the-sandbox-envelope-around-every-agent-turn).
- **Telemetry stays on your device.** Turn measurements are device-local
  facts (`ligus facts export --jsonl`); only a bounded daily outcome summary
  syncs. See [Telemetry](../concepts/security.md#facts).

### Phone and always-on hosts

- **Ligus on your phone.** An iOS app that pairs with your own `ligus serve`
  box, directly or through a relay tunnel. What you do as yourself from the
  phone (post, approve or reject, answer an agent's question, clear a
  lockdown, ask a host to apply) is signed by the phone's own key, with your
  passkey where the rules need it. See
  [Ligus on your phone](../features/phone-app.md),
  [Remote access for your phone](../../operate/networking/remote-access.md) and
  [Phone actions](../../operate/reference/serve-api.md#phone-actions).
- **serve and worker boxes are their own members.** A `ligus serve` or
  `ligus worker` box joins as its own **service member**, never as a
  person's device: at most Contributor, its votes never count as a person's
  approval, and it admits no one. An Admin admits a box once with a passkey
  (desktop, phone, or `ligus admit issue`). See
  [Admitting a box](../../operate/ops/admitting-a-box.md) and
  [Running ligus serve](../../operate/ops/serve-box.md).
- **Apply on a host.** **Apply on ‹host›** (phone and desktop) asks one named
  host to apply an approved proposal; only that host applies it, once. There
  is no auto-apply. See [Apply on a host](../../operate/ops/serve-box.md#apply-on-a-host).
- **Repos from the phone.** `ligus serve` lists the host's checkouts, clones
  on demand, switches a chat's repo, pulls, and opens a pull request from the
  chat branch. See [Repos on the home host](../../operate/ops/serve-repos.md).
- **Typing indicators**, sealed under the workspace key so the relay doesn't
  see who is typing where. See [Serve API: typing](../../operate/reference/serve-api.md#typing).
- **Host dashboard.** A configurable status page for a box. See
  [Host dashboard](../features/dashboard.md).

### Install and deploy

- **Start at login on macOS (optional).** `deploy/local/ligus-local start
  --launchd` installs a per-user LaunchAgent for a `serve` or `worker` box, so
  it starts when you log in and is restarted if it exits; `stop --launchd`
  removes it and `status` shows whether it is installed and running. The
  plist runs `ligus` without a shell and carries no secrets. See
  [Start at login on macOS](../../operate/deploy/personal-fleet.md#start-at-login-on-macos).
- **Homebrew.** Formula `ligus`, cask `ligus-app`, tap `langstroth-ai/ligus`;
  or the `install.sh` one-liner for the CLI, which checks the release's
  SHA-256 file. See [Install with Homebrew](../getting-started/homebrew.md).
- **Deployment guides** for each supported shape, from a
  [solo laptop](../../operate/deploy/solo.md) to a
  [personal fleet](../../operate/deploy/personal-fleet.md), a
  [team on a relay](../../operate/ops/deployment.md) and
  [Kubernetes (preview)](../../operate/deploy/kubernetes.md) with Helm charts
  and a container image for the `ligus` binary (`deploy/container`). See
  [Choose a deployment](../../operate/deploy/index.md).
- **Security and operations docs.** A [security model](../../operate/security/index.md),
  [threat model](../../operate/security/threat-model.md),
  [runbooks](../../operate/security/runbooks.md),
  [operations](../../operate/ops/operations.md), and references for the
  [CLI](cli.md), [environment variables](../../operate/reference/env-vars.md),
  [serve API](../../operate/reference/serve-api.md) and [settings](settings.md).

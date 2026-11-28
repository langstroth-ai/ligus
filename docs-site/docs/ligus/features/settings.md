# Settings

Ligus's Settings view has a **grouped, searchable sidebar** so the page
isn't one overwhelming scroll — only the active section mounts. The
groups are **You** (Account, Appearance), **This workspace** (Folder &
Git, Team sync, Schedules), **Agents** (Models & runtimes, Tools & MCP,
Permissions), and **Advanced** (Updates & data, Diagnostics, Danger
zone). A **Search settings** box at the top filters the nav by section
name *and* by the controls a section contains — typing `api key`,
`dark mode`, `relay`, or `bypass` jumps you to the right place.

Every section **applies its changes immediately** (no Save button).
Workspace-scoped config (runtimes, MCP servers) is written back to
`ligus.config.toml`; connection settings persist to a `settings.json` in
the app data dir; the theme is stored locally per device.

![Settings, Profile: linked accounts and My devices, with Add a computer (demo data)](../images/desktop-settings-profile-light.png#only-light){ width="800" loading=lazy }
![Settings, Profile: linked accounts and My devices, with Add a computer (demo data)](../images/desktop-settings-profile-dark.png#only-dark){ width="800" loading=lazy }

## Profile

- **Profile** — your **display name**, an optional **picture** and your
  **git email** (the commit author for work an agent does on your behalf;
  members of your workspaces can see it). No provider signs you in. Your
  name and picture are your **profile** in every workspace you're in: a
  change shows for everyone, in past messages too. A picture is a PNG, JPEG
  or WebP of 16 KB at most (the desktop shrinks it for you). Change them on
  a desktop: a linked computer, a box or a phone can't.
- **Safety code** — the fingerprint of your account key, as eight groups of
  four hex characters, to compare out of band with people you invite. See
  [Identity & devices](../concepts/identity.md#safety-code).
- **Linked accounts** — optionally show the members of chosen workspaces
  that you are also someone on GitHub, GitLab, Codeberg, Forgejo or Gitea,
  proven with an SSH key the forge lists for you. Also
  here: **Contact forges to check linked identities** (on by default) and the
  self-managed forges this device may ask. See [Linked accounts](linked-identities.md).
- **My devices** — devices that act as you in this workspace. **Rename…**
  on a row gives it a name every member sees ("Ana's MacBook Air"); a box
  names itself after its `--label`, and a linked computer after its link
  request. A computer you approved that never redeemed its code says
  *Approved on …, it hasn't written anything yet* and offers **Remove**.
  **Add a
  computer** takes the link request another computer of yours shows (`lt`,
  `ligus devices request`): compare the fingerprint, confirm with your passkey,
  and paste the grant it shows on that computer, so it acts as you instead of
  as a separate member. See [Add a computer](linked-devices.md). A `ligus
  serve` box isn't added here: it joins as its own member (workspace menu →
  Admit a box).
  **Add a phone** shows a QR code that the phone app scans (**Settings → This
  phone → Add to my devices**); compare the fingerprint and confirm with your
  passkey. **Can't scan?** takes the phone's code as text instead. See
  [Add your phone](phone-app.md#add-this-phone).
- **Privacy → Share typing status** (on by default) — show people in a chat
  when you're typing. Only that you're typing is shared, sealed under the
  workspace key; never what you type. Off, this device only ever says "not
  typing". The phone app has the same switch. See
  [Security](../../operate/security/index.md#encryption).
- **Privacy → Let members use @ligus on this device** (off by default) — in a
  chat with other people, let them use `@ligus`, which runs on this device
  and your account. Off, it answers only you. Not synced. See
  [Who can use your agents](agent-access.md#ligus-the-built-in-assistant).

## Appearance

- **Mode** — `Auto` (follows your OS light/dark setting, any platform),
  `Light`, or `Dark`.
- **Theme** — the accent family: **pollen** (the gold default),
  **studio** (neutral graphite), **harbor** (ocean blue), **meadow**
  (green), **slate** (cool, low-chroma blue-grey), or **obsidian**. Most
  themes have a light and a dark variant and the mode picks which;
  **obsidian is a true dark, monochrome theme** — a neutral near-black
  glass with an achromatic silver accent that stays dark regardless of
  the Mode toggle.
- **Import a theme… / Export current theme** — bring your own colours as
  a theme file (TOML or JSON, the same file lt reads), or save the theme in
  use as a starting file. Imported themes are listed under the palettes,
  with **Rename** and **Remove** in the row menu, and stay on this device.
  See [Themes](themes.md).
- **Density** — how much room rows, cards and messages take:
  **Compact** (shorter rows, tighter turns, a slightly smaller message
  body), **Comfortable** (the default) or **Spacious** (taller rows, more
  space between turns). Stored on this device. It is separate from zoom
  (⌘+ and ⌘−), which scales everything.

## Folder & Git

Each workspace links to a **folder on this device** — its code directory,
which drives the Code view, the Diff canvas, and git integration. Set it
with the **Choose folder…** button (a native OS directory picker) or by
typing/pasting a path. The section also shows a one-line git status
(current **branch** + **changed-file count**) and an **Open in editor**
shortcut.

The folder link is **per workspace and local to this device** — it's
never synced, so teammates in the same workspace each point it at their
own clone. Switching workspaces switches the folder the editor/terminal/
Diff and agents operate on; a workspace you haven't linked yet shows a
**link-a-folder** prompt in the Code/Diff canvas (you can still chat,
review, and drive remote agents while it's unlinked). See
[Workspaces](../concepts/workspaces.md#code-directories-per-chat-worktrees)
and [Git integration](git.md).

## Team sync

Everything needed to share a workspace, editable at runtime (applied
within a few seconds, no restart). Teams are normally created/joined
from the **workspace rail's ＋ button** (which generates the room, key,
and a shareable code); this tab is the manual/advanced surface and the
relay configuration:

- **Relay URL** — peers on the same relay + room converge. Blank =
  local-only. Use the **https base origin** (e.g. `https://relay.example`)
  — no `/v1` suffix, no `wss://`. **Use the community relay** fills in
  `https://relay.langstroth.ai`, the free, best-effort
  [community relay](../../operate/networking/community-relay.md).
- **Relay access token** — needed for a relay that gates access with
  tokens, and to claim a workspace on a relay with membership on: there the
  token has to name your account (an `hrt1` token with a `sub`, see
  [Claiming a workspace](../../operate/ops/server-side-membership.md#claiming-a-workspace)).
  Leave it blank otherwise (the default).
  Sent as a bearer on every relay request; never echoed back. If your
  token was issued as `name:token`, either form works — Ligus sends just
  the token part.
- **Test connection** — actually hits the relay and reports
  **Connected**, **Unauthorized** (reached it, token rejected), or
  **Unreachable**. The status card above it only means a URL is
  *configured*; the probe is the source of truth for whether it works.

Under **Advanced — set room & key by hand**:

- **Room** + **Workspace key** — the room id and the shared passphrase
  → end-to-end encryption; the relay sees only ciphertext (status shows
  `🔒 encrypted`).
- **Relay issuer key** — an Ed25519 seed (64-hex) that lets you **mint
  agent identity tokens locally** for the People pane's *Add a remote
  agent* flow. It's stored on this device only and never leaves it; the
  field shows `✓ set` once configured (enter a new value to replace it).
  See [Remote agents](#remote-agents-people-pane).

See [Self-hosting a relay](../../operate/networking/self-host.md) and the
[team deployment guide](../../operate/ops/deployment.md).

### Relay access tokens

Ligus doesn't manage relay users or tokens. If your relay requires an
access token, the relay operator issues one (see
[Self-hosting a relay](../../operate/networking/self-host.md)) and you
paste it into the **Relay access token** field above.

## Workspace rules

What every device accepts on this workspace. Below rules 4 the Owner sees
**Upgrade workspace rules**: the readiness list (devices on Ligus before
0.5 by name and build, which go read-only there; agents whose host wouldn't
count; members with no account; model server addresses kept on the
workspace), **What changes** behind a disclosure, and one button that asks
for the passkey once and can't be undone. On rules 4, agents that still name
a runtime are listed with **Pin**. See [Workspace rules](workspace-rules.md).

## Schedules

Define **scheduled agents** — recurring runs that kick off a chat turn
on a cron-like schedule without you present. Each schedule has a
**label**, the **prompt** to send each run, a cadence (**Every**
interval or **Daily at** a time), and an optional runtime to pin it to
(defaults to the chat's primary runtime). See
[Scheduled agents](scheduling.md) for the full walkthrough.

## Models & runtimes

LLM access is organized as a hierarchy:

- **Providers** — `anthropic`, `openAI`, `openRouter`, `ollama`,
  `azure` (Azure OpenAI), `custom`, plus the CLI coding agents: `claude`
  (**Claude Code**), `pi`, `aider`, **Codex** (OpenAI's `codex exec`),
  **Google Antigravity** (`agy`), **GitHub Copilot** (`copilot`), and
  **Hermes** (any stdin-driven CLI — set the executable + flags on the
  runtime). API providers hold their **own API key** and optional base URL
  (so multiple providers can have distinct keys, including any generic
  OpenAI-compatible endpoint); the CLI agents use their own login and need
  no key.
- **Models (runtimes)** — a model on a provider (id + capability flags).
  The model field offers the harness's own list and says when a typed
  model isn't on a complete one; a chat can also pick its model from the
  route pill ([Picking the model for a chat](../getting-started/configuring-a-runtime.md#pick-a-model)).
  The add form includes an optional **Context window in tokens** — set
  it for Ollama/custom models whose window Ligus can't infer from the
  name; the [context planner](voice-and-slash.md) budgets against it.
  Ollama runtimes add **Keep alive** (how long the server keeps the
  model loaded) and **Let the model think** (reasoning models: the
  chain of thought streams live but never lands in the transcript).
- **Models panel (Ollama)** — each Ollama runtime row has a **Models**
  panel showing what is installed on that server (parameters, quant,
  size, a *loaded* badge with the context window it is loaded at).
  **Pull** a new tag or **Update** an installed one with per-layer
  progress, **Cancel** a pull (the server keeps downloaded layers, so a
  retry resumes), **Remove** a model, or **Use** one to point the
  runtime at it — no ssh + `ollama pull` needed.
- **Agents** — reusable personas (name + model/runtime + role +
  instructions) you can attach to any chat.

The default runtime is the **`claude` CLI** — no API key needed, it uses
your Claude subscription. See
[Configuring a runtime](../getting-started/configuring-a-runtime.md).

### Detect my setup

**Detect** (then **Re-scan**) sweeps the machine for providers that
would answer right now — a running Ollama, CLI agents already on your
`PATH` (`claude`, `codex`, `aider`, …), and API keys present in the
environment — and offers to add each with one click, so a fresh install
is usable without hand-typing config.

### Subscription CLIs

The **Subscription CLIs** panel (under **LLM providers**) looks for the
Google Antigravity CLI (`agy`) and the GitHub Copilot CLI (`copilot`), so
a Google AI Pro / Ultra or Copilot subscription can drive a runtime. For
each it shows **not installed** (with the install command) or the installed
version and sign-in state: **signed in**, **not signed in** (with the
command to run in a terminal: `agy` or `copilot login`), or **sign-in not
checked**. Antigravity's Google sign-in lives in the OS keychain, so it
reads *not checked* until you click **Check Google sign-in**, which runs the
read-only `agy models`. **Add runtime** adds a keyless runtime in one
click, optionally with a model. A `gemini` binary is listed only to explain
that Gemini CLI no longer serves Google AI Pro / Ultra sign-ins. See
[Configuring a runtime](../getting-started/configuring-a-runtime.md#subscription-clis-google-antigravity-and-github-copilot).

### Testing a provider or model

- On a provider row, **Test** makes a real call and reports whether the
  stored key/endpoint actually works.
- In the add-model form, **Test** validates the just-typed key/base URL
  *before* you save, and **Add model** then registers a ready-to-use
  runtime for that model.

### Portable config

**Export** dumps this machine's runtimes + MCP setup as a `ligus.toml`
you can copy to another device or share with a teammate; **Import**
stamps a pasted `ligus.toml` onto this machine (**additive** — it adds
what's missing and never deletes what you already have). No API keys or
tokens are included: a runtime records only the env var its key is read
from. Agents aren't part of it; add them with **Add an agent**. An older
export's agents are skipped on import.

### Context commands

The instructions behind `/summarize` and `/compact` are editable here —
blank uses the built-in default (shown as the placeholder). The
`/summarize` instruction also guides the automatic summarization of
overflowed history. See
[Managing context](voice-and-slash.md#customizing-the-instructions).

## Tools & MCP

Install / enable / remove **Model Context Protocol (MCP) servers**. An
installed server stays **inert until you enable it** — enabling is what
launches the command or opens the connection, and only enabled servers
expose their tools to agents. Two transports: `stdio` (Ligus spawns the
binary) and `http`. See [MCP servers](../addons/mcp.md).

## Permissions

How agents are allowed to touch files — and this differs **per agent
type** (Claude Code is not the only agent that edits files):

- **`claude` permission mode** — `Read-only` (default; proposes edits
  but blocks writes), **Edit files** (can write files), **Edit files & run
  commands** (also build, test and git through the shell), or **Bypass all
  permissions** (edits, commands and every tool). Injected as
  `--permission-mode` into the `claude` CLI, which runs headless and
  can't show an interactive prompt. Because bypass mode grants
  unattended file **and** shell execution, switching into it pops a
  **confirmation dialog** first — the same friction a data reset gets.
- **`agy` and `copilot`** (the subscription CLIs) follow the same setting,
  mapped to each CLI's own flags; anything a tier doesn't grant is denied,
  since there's no one to prompt. See
  [Bring-your-own agents](../addons/byo-agents.md#subscription-clis-google-antigravity-and-github-copilot)
  for the exact flags.
- **aider / pi** gate via their own flags.
- **API/MCP-backed agents** can only call the MCP tools you've
  **enabled** on the Tools & MCP tab — an installed-but-disabled server
  is inert, so nothing runs until you turn it on.

## Flow

Per-device settings for [Flow](flow.md):

- **Routing**: route un-addressed messages **Off**, in **Shadow** (the
  default: log what the router would pick, the primary still answers) or
  **Live**; the outcome half-life; the cool-down after a failure (15 min,
  0 = never); the exploration share bounds (5–30 %).
- **Review queue**: the queue limit (5) and long-wait threshold (2 h); the
  stop-upstream note (**On**, **A/B** in half the chats, or **Off**); **Ask
  more reviewers** and **Take queued stages when idle** (both off by
  default).
- **Task board**: let agents pick up work on their own (off by default).
- **Workspace marks**: show marks in agent context (on).
- **Worktrees**: reuse turn worktrees (on).
- **Hand-offs**: hand-off cards and context parcels (on).

## Updates & data

- **Check for updates** — reports the version you're on and checks
  whether a newer **signed** build is available, then installs it.  When an
  in-place update can't be applied, Ligus links to the release on GitHub
  instead. A newer release also shows as **Update available** in the status
  bar; click it for **Download**, **Later** (until the next launch) or
  **Skip this version**.
- **Export data** — download a copy of this device's local data.

## Diagnostics

Open the **log file** (structured logs plus panics) — useful when
something misbehaves silently. Set `LIGUS_LOG=debug` in the environment
before launching for verbose output while you're chasing an issue.

## Danger zone

**Reset local data** wipes this device's chats, identity, keys,
settings, and workspaces, then relaunches Ligus fresh. It's the supported
way to start over — uninstalling leaves data behind. See
[Reset local data](../getting-started/first-launch.md#reset-local-data)
for the per-OS data directories.

## Remote agents (People pane)

Not a Settings tab, but the counterpart to the **Relay issuer key**
above: the right rail's **People** pane has a **Remote agents** section
(shown once a relay is connected). Enroll a remote agent by handle and
Ligus **generates the exact command** — with the relay URL, room, a
minted agent token, and workspace credentials filled in — for a
developer to copy and run on the agent's box. Detached agents run on
**workspace credentials**, never a personal API key. See
[Headless agents](../../operate/concepts/headless-agents.md).

## Notes

- New chats are **auto-titled** from the opening exchange using the
  chat's primary runtime — no setting required. Rename any chat with the
  ✎ pencil next to its title.
- **System tray / menu bar** — the tray icon's menu jumps straight to
  **People**, **Team & Relay Sync** (this page's Team sync tab), and
  **Settings**, besides showing/quitting the app. Its tooltip says how many
  things need you ("Ligus · 2 need you"), the same number as the
  [Inbox](inbox.md).
- **Command palette** — **⌘K** finds anything by typing part of it (the
  match is fuzzy, so "nw ch" finds "New chat"): chats, workspaces,
  settings tabs, panes, proposals waiting on a decision (**Review "…"**),
  the workspace's agents (**@name** starts a message to it), **Add a
  phone…**, **Open Inbox**, **Recent errors** and **Keyboard shortcuts**.
  With nothing typed it lists what you ran recently, then what needs you,
  then everything else; what you ran recently also ranks higher as you
  type.
- **Keyboard shortcuts** — **⌘/** (or "Keyboard shortcuts" in the
  palette) opens a sheet of every shortcut, grouped General, Chat, Panes,
  View and Code. It's built from the same list the keys use, so it is
  always current. **⌘⌥1** to **⌘⌥0** open the ten panes, and **Esc** stops
  a running turn unless you're typing in a field with text in it (your
  draft is never touched). On Windows and Linux, ⌘ is Ctrl and ⌥ is Alt.
- **Tooltips** — buttons and controls show a short themed tooltip on
  hover and when they get keyboard focus, with the shortcut as a small key
  chip where there is one. Menus (such as the **⋯** more-actions menus)
  work from the keyboard: arrow keys to move, Enter to pick, Esc to close.
- **Focus mode** — **⌘.** (or "Focus mode" in the ⌘K palette) hides
  everything but the chat and the message box: the workspace rail, the
  sidebar, the tools rail and the status bar. A lockdown or dearth-mode
  banner still shows. The chat header keeps one quiet "N need you" badge
  that opens the [Inbox](inbox.md), and **Leave focus**. Press **⌘.** again
  to bring everything back as it was; **⌘B**, **⌘J** or opening a pane also
  leaves focus mode. Focus mode lasts until you leave it or restart.
- **Sidebar and tools rail** — collapse the sidebar with **⌘B** (or the
  panel button at the bottom of the workspace rail) and the tools rail with
  **⌘J** (or the **Tools** button in the chat header). Both persist across
  restarts, and both live in the ⌘K palette as "Toggle sidebar" / "Toggle
  tools rail".
- Workspace config round-trips through the TOML encoder, so hand-edited
  and GUI-edited `ligus.config.toml` files stay compatible (comments
  aren't preserved). See
  [`ligus.config.toml` reference](../../operate/reference/config-toml.md).

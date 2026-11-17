# What's new

**Hive is now Ligus (v2.0.0).** Ligus is short for *ligustica*, the Italian
honeybee. Entries for 1.18 and earlier keep the name Hive.

Changes since 1.15, newest first, with links to where each is documented.
Full release notes are on the
[GitHub releases page](https://github.com/ligus-ai/ligus/releases).

## 2.0

- **Agents can't lend a turn authority.** A turn an agent starts (a
  hand-off, an @mention cascade, a workflow stage) runs at the lowest role
  of the humans who started the chain, on every host. Before, an agent's
  reply your device signed counted as your own message, so a Contributor's
  request handed on to another agent ran at your Owner tier. Agent replies
  with no human behind them now run read-only. See
  [the author gate](../concepts/security.md#author-gate).
- **Nobody can post as another member.** An event whose actor stamp names
  a member's account from a device not certified for that account is now
  quarantined everywhere, and the author gate gives it no role. Before, a
  Contributor could stamp the Owner's id and act as the Owner, including
  promoting themselves. Trigger chains are also judged only against the
  chat's own workspace. See
  [the author gate](../concepts/security.md#author-gate).
- **Automatic board pickup runs with no role.** A task the board picks up
  on its own (a proposal's or failed stage's title, written by whoever
  filed it) no longer runs as you at your Owner tier; it runs below every
  floor. Picking an item up yourself is unchanged. See
  [the author gate](../concepts/security.md#author-gate) and the
  [threat model](../security/threat-model.md#known-limitations) for what
  human-clicked actions still carry.
- **No sandbox, no local agents.** A host without an OS sandbox (Windows,
  Linux without bubblewrap or Landlock, `LIGUS_SANDBOX=off`) now refuses
  read-only CLI agent turns too, plus `run_command`, pre-flight tests and
  stdio MCP servers; HTTP models answer there without local tools. Windows
  isn't supported for agent turns. See
  [the OS sandbox](../concepts/security.md#os-sandbox).
- **Typing indicators everywhere, now encrypted.** "X is typing" works
  across the desktop and phones: the phone tells its `ligus serve` box,
  which publishes for that chat (as you, once the box is your device), and
  shows who else is typing. Presence blobs on the relay are now sealed under
  the workspace key, so the relay no longer sees who is typing in which chat.
  New **Share typing status** switch (on by default) on desktop and phone.
  See [Phone app](../features/phone-app.md#chats),
  [Serve API: typing](serve-api.md#typing) and
  [Security](../security/index.md#encryption).
- **Phone shows which device answered** a primary `@hive` reply ("This
  device", "Your other device", "<name>'s device").
- **Claude Code turns get only what the turn needs.** Ligus starts Claude
  Code without cloud routines, remote triggers, scheduled tasks, background
  monitors, push notifications, cross-session messaging or workflows, and
  without your settings, hooks, plugins, skills and MCP servers; Ligus's own
  tools and the chat's MCP servers are pre-approved, so they work in
  non-interactive turns. Turn your own setup back on per workspace in
  **Settings → Guards → Your own agent setup**. Codex turns skip your
  `config.toml` the same way, and Copilot turns run without the built-in
  GitHub MCP server. See
  [What a Claude Code turn can and can't do](../concepts/security.md#claude-code-turn).
- **Egress presets.** Opt-in sets of hosts for a chat's egress allow-list,
  off by default: `package-registries` (npm, crates.io, PyPI, GitHub) and
  `claude-connectors` (your claude.ai connectors, Claude Code only).
  Switch them per workspace or per chat in **Settings → Guards → Network
  presets**, or with `LIGUS_SANDBOX_EGRESS_PRESETS`; a lockdown or dearth
  mode overrides them. See [Egress presets](../concepts/security.md#egress-presets)
  and the [Sandbox defaults](../concepts/security.md#sandbox-defaults).
- **A Linux sandbox without user namespaces.** Where bubblewrap can't
  create a user namespace (Ubuntu 24.04's AppArmor restriction), agent
  turns now run in a Landlock + seccomp tier instead of being refused, and
  an optional AppArmor profile restores bubblewrap. `ligus doctor` shows
  the tier. See [The Linux agent sandbox](../ops/linux-sandbox.md).
- **Passkeys for admin changes.** Under workspace rules v2, membership and
  role changes, device revocation, key rotation, guards, zones, workflow
  definitions, agent hosts and human-gated approvals need a passkey (Touch
  ID, Windows Hello, a phone, a YubiKey), verified on every device. Register
  passkeys and a recovery key in **Settings → Passkeys & keys**; the phone
  confirms votes through your box with Face ID. See
  [Passkey presence](../security/index.md#presence).
- **Signed key rotations on every host.** Rotating the workspace key is a
  signed event on the workspace log, sealed to keys each device published
  itself; `ligus serve`, `ligus worker` and the CLI follow rotations too.
  **Settings → Passkeys & keys → Rotate workspace key**.
- **Secrets in the OS secret store.** Keys and tokens move out of
  `settings.json` and `config.toml` into a dedicated macOS keychain, the
  Secret Service or `systemd-creds`. See
  [Key storage](../concepts/identity.md#key-storage).

- **Agents get typed tools.** Agents file proposals, hand off, leave marks,
  read the workspace and ask you questions through the
  [Ligus tools](../addons/ligus-mcp.md) instead of `[[…]]` reply text. A
  hand-off starts the receiving agent right away, on whichever host runs it.
  A question pauses the agent's turn until someone answers, from the desktop
  or the phone. Agents sign with their own keys, and can only propose.
- **Subscription CLIs over ACP.** Copilot CLI, Antigravity and Codex (and
  Claude Code with an API key) run over the
  [Agent Client Protocol](../addons/acp.md). Their permission requests follow
  your tier or come to you as a question. The headless adapters remain the
  fallback.
- **Review shows exactly what Implement does.** Each proposal shows its kind,
  its author and signature, config impact and gates. Implement previews the
  exact command or instruction, the agent (the one that proposed it), the
  runtime, the sandbox and the network. A command proposal runs its exact
  argv, sandboxed and offline, with no model involved.
- **Gemini API and MLX presets**, and a [runtime matrix](runtime-matrix.md)
  that's run before every release.
- **Hive is now Ligus.** Ligus is short for *ligustica* (*Apis mellifera
  ligustica*, the Italian honeybee). The default agent is still `@hive`, and
  2.0 clients use the same relays (`/v1` and `hrt1` tokens are unchanged).
- **v2.0 starts fresh.** The CLI is `ligus` only (no `hive` command), settings
  are `LIGUS_*` only, the config file is `ligus.config.toml` and workspace
  state lives in `.ligus/`. Nothing from Hive 1.x is migrated: you create new
  workspaces, invite teammates again and re-pair phones. In exchange, 2.0
  never reads, modifies, moves, renames, copies or deletes any 1.x file: the
  desktop's `com.hive.desktop` folder, the CLI's `~/.hive`, each repo's
  `.hive/` (with its chat checkouts and `hive/chat/*` branches) and
  `hive.config.toml` stay exactly where they are, so a 1.x install keeps
  running side by side. 2.0 refuses a data dir that is, or links to, 1.x
  data. A `HIVE_*` variable that is still set is ignored, with a one-time
  warning naming the new spelling. Setting up 2.0 and removing 1.x when you're
  done: [Upgrading from Hive 1.x](../deploy/upgrade-from-hive.md).
- **Deployment and security docs.** [Choose a deployment](../deploy/index.md)
  with guides for each supported shape, a [security model](../security/index.md),
  [threat model](../security/threat-model.md) and [runbooks](../security/runbooks.md),
  [Operations](../ops/operations.md), and references for the
  [CLI](cli.md), [environment variables](env-vars.md),
  [serve API](serve-api.md) and [settings](settings.md).
- **Every device judges history the same way.** One authorization engine
  judges each event at its position on the workspace config log: votes count
  at the voter's role when cast, for the exact content they name; proposal
  status is derived from counted votes; later promotions and demotions never
  rewrite earlier verdicts; events whose position hasn't synced wait instead
  of being dropped. Every author must be a member at the event's position,
  and joining needs an Admin's single-use invite. Envelopes are kept and
  re-sent as the bytes received, so a newer build's fields survive.
  [Authorization](../concepts/security.md#authz-at-position)
- **Agent processes run in an OS sandbox.** Seatbelt on macOS, bubblewrap +
  Landlock + seccomp on Linux: no reads of Ligus's data dir or credential
  dirs, writes only in the worktree, and the network only through a per-turn
  egress proxy. Children start from an allow-listed environment.
  [The OS sandbox](../concepts/security.md#os-sandbox)
- **One author gate on every host.** A write-capable turn needs an Admin's
  message and a read-only one a Contributor's, at the author's role when they
  posted. [The author gate](../concepts/security.md#author-gate)
- **Trust this workspace.** Runtimes and MCP servers a repository supplies
  stay inert until you enable them on your device (`ligus trust`).
  [Trust this workspace](../concepts/security.md#workspace-trust)
- **Telemetry stays on your device.** Turn measurements are device-local
  facts (`ligus facts export --jsonl`); only a bounded daily outcome summary
  syncs. The export also carries guard signals and content-free state facts
  (reply, proposal, workflow, roster, host and lease state as this device's
  fold derived it) with a resumable cursor, so an observability tool never
  needs to open the database. [Telemetry](../concepts/security.md#facts)
- **Phone approvals carry your passkey.** A `ligus serve` box enrolled as
  your device takes votes, rejects and lockdown clears from the phone only
  with your passkey over them (`presence/prepare`, `presence/commit`); folds
  drop its votes without one. [Securing the box](../ops/serve-box.md)
- **Organization sign-in.** `ligus auth login` and Settings → Team sync sign
  in through [ligus-auth](../deploy/ligus-auth.md) with your identity
  provider (setups for [Pocket ID](../deploy/ligus-auth-pocket-id.md) and
  [Dex](../deploy/ligus-auth-dex.md)) and keep a short-lived relay token
  fresh.
- **Desktop.** The app is `Ligus.app`. Theme and pane layout reset once, and
  macOS asks again for folder and notification access. An auto-updated app
  may still be at `/Applications/Hive.app`; you can rename it to `Ligus.app`.
- **Homebrew.** Formula `ligus`, cask `ligus-app`, tap `ligus-ai/ligus`;
  existing `hive-cli` and `hive` installs upgrade to them.
- **Repos from the phone.** `ligus serve` lists the host's checkouts and your
  GitHub repos, clones on demand, switches a chat's repo, pulls, and opens a
  pull request from the chat branch. See
  [Repos on the home host](../ops/serve-repos.md).
- **Fork-aware pull requests.** For a fork with an `upstream` remote, chat
  branches start from upstream's default branch, pulls come from upstream,
  and PRs are pushed to your fork under a readable branch and opened upstream
  as drafts. Pushes that would land on upstream are refused.
- **Rendered Markdown diffs.** `GET /v1/proposals/{id}/file` returns one file
  of a proposal whole (old or new side), so the phone app can render Markdown
  changes instead of showing raw diff lines.
- **Kubernetes design and a container image.** A design for running the relay,
  `ligus serve` and sandboxed turn executors on Kubernetes, and a Dockerfile
  for the `ligus` binary (`deploy/container`).
- **New addresses.** The project lives at github.com/ligus-ai/ligus and the
  docs at docs.ligus.ai. Old links redirect. Protocol identifiers that contain
  `hive` keep their names; see [Wire protocols](wire-protocols.md#legacy-protocol-identifiers).
- **Workspace guards.** A new Settings → Guards page collects the controls
  that keep a workspace inside safe bounds.
  [Workspace guards](../features/guards.md)
- **Dearth mode.** An Admin can close the workspace's entrance for a hostile
  period: new members and devices need two Admins, agents keep only
  allow-listed MCP servers, and changes to protected paths need a human. Every
  member sees a banner. [Dearth mode](../features/guards.md#dearth-mode)
- **One sandbox envelope for every runtime.** Isolation, protected
  directories, an egress policy and secret redaction now apply the same way to
  Claude Code, Codex, API models and the rest; runtimes Ligus can't confine
  are refused while egress is restricted, and credentials in agent replies are
  masked before they're logged.
  [The sandbox envelope](../concepts/security.md#the-sandbox-envelope-around-every-agent-turn)
- **Chat lockdown.** A prompt-injection marker in a tool result, a credential
  in an agent's diff or an unrequested CI change locks the chat down (no
  network for its agents, a human approval on every proposal) until a member
  clears it. [Chat lockdown](../features/guards.md#chat-lockdown)
- **Coordinator leases for workflow runs.** Exactly one device drives a run,
  renewing a signed heartbeat; another takes over only after it lapses, the
  driver can hand a run over on purpose, and two devices that both try at once
  are fenced by the lease epoch. A `ligus serve` box holds the workspace
  coordinator lease. [Which device drives a run](../features/workflows.md#which-device-drives-a-run)
- **Staggered plan-window throttles.** Near a subscription limit, scheduled
  runs pause first, then workflow stages and agent hand-offs, each agent at a
  slightly different point; your own requests always run.
  [Plan windows](../features/guards.md#plan-windows-staggered-throttles)
- **Hygiene, the patch-leaving rule and growth suggestions.** Leftover
  worktrees are swept and stale proposals set aside; long tool loops check in
  when they stop making measurable progress; and the workspace suggests a
  host, worker or agent only when its occupancy calls for one.
  [Hygiene](../features/guards.md#hygiene)
- **Proposal sets.** Ask for options and agents offer competing
  alternatives; other agents assess each one blind, their support fades
  unless renewed, a quorum of scouts picks a front-runner and stops its
  rivals, and you decide with a pre-flight (tests, base, hosts) before
  applying. A fast to careful dial sets how much agreement is needed. See
  [Exploring options](../features/proposal-sets.md).
- **Flow.** Hand-off cards and context parcels when one agent passes work to
  another; un-addressed work routed by measured outcomes (in shadow mode
  first) with cool-downs after failures; backpressure at the review queue; a
  task board for work nobody was asked to do; workspace marks (claims, locks,
  review markers); path zones that raise the review bar where a change lands;
  reused turn worktrees; and a home beacon. See [Flow](../features/flow.md).

## 1.18

- **Agent-authored workflows.** The visual workflow editor and its presets
  are gone. Agents write workflows: a definition (or a revision of one)
  arrives in the Review pane as a proposal, and approving it adds it. The
  Workflows pane is read-only, with Run, Pause/Resume, Delete and *Ask an
  agent to change this*. Local-model agents get a `propose_workflow` tool
  that checks the definition in the same turn. Existing workflows keep
  working. See [Agentic workflows](../features/workflows.md).
- **Google Antigravity and GitHub Copilot runtimes.** Use your Google AI
  Pro/Ultra or Copilot subscription through the `agy` and `copilot` CLIs;
  Settings → Models detects whether each is installed and signed in. See
  [Configuring a runtime](../getting-started/configuring-a-runtime.md) and
  [Bring-your-own agents](../addons/byo-agents.md).

## 1.17

- **Host dashboard.** `hive serve` serves a dashboard to the phone: a
  built-in Hive section (agents, proposals, workflows, turns, host, relay and
  push status), your own panels from `dashboard.toml`, panels backed by MCP
  tools, a table panel kind, and `hive dashboard check | metrics | default`.
  See [Host dashboard](../features/dashboard.md).
- **MCP tools at the local-model tool cap.** Local agents no longer silently
  lose MCP tools at the 16-tool limit: slots are kept for MCP tools, tools
  you name are preferred, and dropped tools are logged. MCP errors show the
  server's own message. See [MCP servers](../addons/mcp.md).

## 1.16.1

- Every user message a sync brings in gets an answer, not only the chat's
  last one.
- Push previews are plain text.
- `hive remote status` shows how long the relay link has been up and
  notices a `hive serve` that stopped; `hive --version` prints the version.
  See [Troubleshooting](../ops/troubleshooting.md).

## 1.16

- **Hive on your phone, from anywhere.** `hive serve` plus the relay tunnel
  gives the phone app remote access without a VPN: `hive pair` prints the
  pairing link, `hive phones` lists and revokes phones, `hive remote`
  manages the link. A self-hosted relay can run the tunnel and send push
  with its own APNs key. See [Remote access for your phone](../networking/remote-access.md).
- **On the phone:** people, channels and chat details; proposal review
  with votes (who and role), a refinement thread and *request changes*;
  push for mentions and workflow approvals; the host's theme through
  *Match host*. See [Hive on your phone](../features/phone-app.md).
- **The serve box as your own device.** `hive serve enroll` makes the box
  one of your devices instead of a separate member. See
  [Running hive serve as your own device](../ops/serve-box.md).
- **Sync privacy fix.** A relay room now carries only its own workspace;
  events from other workspaces are dropped and purged on upgrade. Read the
  [upgrade note](../ops/serve-box.md#what-to-expect-and-do-when-upgrading-from-115-or-earlier)
  if you're coming from 1.15 or earlier.
- **Presence-based host liveness.** Whether a host is online rides the
  relay's presence channel instead of heartbeat events in the log. See
  [Host liveness](../ops/serve-box.md#host-liveness).

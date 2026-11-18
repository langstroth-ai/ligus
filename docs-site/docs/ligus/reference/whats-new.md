# What's new

Changes in each release, newest first, with links to where each is documented.
Full release notes are on the
[GitHub releases page](https://github.com/langstroth-ai/ligus/releases).

## 0.1.0

- **The phone acts as you with its own key; one host applies.** Since the
  box is its own service member, what you do as yourself from the phone
  (post, approve or reject, answer an agent's question, clear a lockdown,
  ask a host to apply, withdraw that request) is signed by the phone's own
  Secure Enclave key, with your passkey where the rules need it; the box
  drafts the event, the phone checks every byte before signing, and the box
  can't change it. **Apply on ‹host›** (phone and desktop) asks one named
  host to apply an approved proposal: only that host applies it, once, and
  reports the commit or why it couldn't; nobody else applies it while the
  request is open. Auto-apply on approval (`LIGUS_SERVE_APPLY_ON_APPROVE`,
  the chart's `serve.applyOnApprove`) is removed. The desktop's People pane
  disables what the rank rules refuse (an Admin cutting an Admin, a
  non-founder cutting an Owner, anyone cutting the founder, a box above
  Contributor) and says why. The box's own person-action routes (`/v1/proposals/{id}/approve` and
  `/reject`, `/v1/chats/{id}/lockdown/clear`, `/v1/asks/{id}/answer`, and
  `presence/prepare` for a vote or a clear) answer 410 and name the phone
  action to use. See
  [Phone actions](../../operate/reference/serve-api.md#phone-actions) and
  [Apply on a host](../../operate/ops/serve-box.md#apply-on-a-host).
- **serve and worker boxes are their own members.** A `ligus serve` or
  `ligus worker` box joins a workspace as its own **service member**, never as
  a person's device: at most Contributor, its votes never count as a person's
  approval, and it cuts and admits no one. An Admin admits a box once with a
  passkey: on the desktop (workspace menu, **Admit a box…**, from the box's
  join request or as a code for a Kubernetes Secret), from the phone with its
  own device key, or headlessly with `ligus admit issue`. A box syncs first
  and refuses to start without a copy of the workspace, so it never founds a
  competing one. `ligus serve enroll`, `LIGUS_SERVE_OWNER` and the chart's
  `serve.owner` were removed and stop with a migration error. The chart can
  now run serve and workers in one workspace (`serve.admission`,
  `worker.instances[].admission`). Only the holder of an admission code can
  redeem it (a redeem key's signature, bound to the box's device key when it
  was issued for a join request), and every Admin's desktop closes an
  admission once it is redeemed or lapses, so no later or backdated
  redemption admits anyone. See
  [Admitting a box](../../operate/ops/admitting-a-box.md).
- **A community relay to start with.** Onboarding, **Add workspace → Create**
  (when no relay is set) and **Settings → Team sync** offer
  `relay.langstroth.ai`, a free, best-effort relay, next to your own relay
  and no relay. It is only used when you pick it, and it sees only
  ciphertext. See [The community relay](../../operate/networking/community-relay.md).
- **A DM invite waits for their device.** Opening a DM with someone whose
  contact id your device doesn't know yet (often right after they joined, or
  from the phone) used to hand you a link and never try again. Ligus now
  holds the invite, sealed to your device's key, and delivers it once their
  contact id arrives: once, only to the devices you had when you opened the
  DM (a changed key shows the unverified-recipient warning instead), for up
  to 7 days. The DM list says it will be delivered when their device is
  reachable, with the link as a fallback. See
  [People, contacts & DMs](../features/collaborators.md#when-their-device-isnt-reachable-yet).
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
  [threat model](../../operate/security/threat-model.md#known-limitations) for what
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
  [Serve API: typing](../../operate/reference/serve-api.md#typing) and
  [Security](../../operate/security/index.md#encryption).
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
  the tier. See [The Linux agent sandbox](../../operate/ops/linux-sandbox.md).
- **Passkeys for admin changes.** On every workspace, from its first event, membership and
  role changes, device revocation, key rotation, guards, zones, workflow
  definitions, agent hosts and human-gated approvals need a passkey (Touch
  ID, Windows Hello, a phone, a YubiKey), verified on every device. Register
  passkeys and a recovery key when you create a workspace (or in
  **Settings → Passkeys & keys**); a self-hosted deployment can name its
  own relying party for the workspaces it creates; the phone
  confirms votes through your box with Face ID. See
  [Passkey presence](../concepts/security.md#passkeys).
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
- **Deployment and security docs.** [Choose a deployment](../../operate/deploy/index.md)
  with guides for each supported shape, a [security model](../../operate/security/index.md),
  [threat model](../../operate/security/threat-model.md) and [runbooks](../../operate/security/runbooks.md),
  [Operations](../../operate/ops/operations.md), and references for the
  [CLI](cli.md), [environment variables](../../operate/reference/env-vars.md),
  [serve API](../../operate/reference/serve-api.md) and [settings](settings.md).
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
  drop its votes without one. [Securing the box](../../operate/ops/serve-box.md)
- **Organization sign-in.** `ligus auth login` and Settings → Team sync sign
  in through [langstroth-auth](../../operate/deploy/ligus-auth.md) with your identity
  provider (setups for [Pocket ID](../../operate/deploy/ligus-auth-pocket-id.md) and
  [Dex](../../operate/deploy/ligus-auth-dex.md)) and keep a short-lived relay token
  fresh.
- **Homebrew.** Formula `ligus`, cask `ligus-app`, tap `langstroth-ai/ligus`.
- **Repos from the phone.** `ligus serve` lists the host's checkouts and your
  GitHub repos, clones on demand, switches a chat's repo, pulls, and opens a
  pull request from the chat branch. See
  [Repos on the home host](../../operate/ops/serve-repos.md).
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
- **Docs in two sections.** The project lives at github.com/langstroth-ai/ligus
  and the docs at docs.langstroth.ai, in two sections:
  [Using Ligus](../index.md) and
  [Operating Langstroth](../../operate/index.md) (self-hosting and
  deploying Langstroth, the platform behind the apps).
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

# What's new

Changes in each release, newest first, with links to where each is documented.
Full release notes are on the
[GitHub releases page](https://github.com/langstroth-ai/ligus/releases).

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

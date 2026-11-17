# Security & trust

Ligus is end-to-end encrypted and the relay is content-blind. This page states
the guarantees you can rely on when a workspace syncs through a relay. A
local-only workspace (no relay) puts nothing on the wire and is unaffected by
everything below.

## E2EE is mandatory for relay sync

Ligus **refuses to push plaintext to a relay**. A workspace configured for sync
but with no workspace key does not sync — the app surfaces a **Sync error**
telling you to configure a key. The relay therefore only ever holds sealed
(E2EE) bodies; there is no "encrypt later" mode that leaks cleartext in the
meantime. The rule holds inbound too: a device that holds the workspace key
never applies an unsealed body from the relay, so a relay can't inject
events into an encrypted workspace. (Plaintext history a workspace left on a
relay before it had a key therefore doesn't appear on devices that fetch it
later.)

## The workspace key never transits the relay

The key is exchanged **out-of-band**:

- The `hivews1:` invite code carries the key *inside the code itself*, so share
  the code over a channel you trust — paste it directly to the joiner. See the
  [invite code format](../reference/invite-payload.md).
- [Inviting a co-member](../features/invites.md#invite-someone-you-already-share-a-workspace-with)
  seals the code to their verified devices instead, so nothing sensitive is
  copy-pasted; the relay only stores the sealed drop.

The old relay-brokered "short code" that published the key to the relay's
pairing store has been **removed**; the relay is never a party to key exchange.

## The relay is content-blind

Every envelope is sealed with ChaCha20-Poly1305 *before* it leaves the device;
the relay stores and forwards ciphertext only. It cannot read your traffic, and
because it never holds the key it cannot help you recover a lost one.

## Authorization is enforced on ingest, not just at authoring

Governance is re-checked when a synced event is **projected**, not only when it
is authored. A membership change, a `MemberRoleChanged`, or a workspace-config
change from an author who lacked the required role is **dropped during
projection** — deterministically, on every device, so convergence is preserved.

A malicious member therefore cannot forge a validly-signed
`MemberRoleChanged{ self: Viewer → Owner }` to seize the workspace: the
signature verifies, but the author's role at that point did not permit the
change, so every peer drops it. Membership is workspace-wide (via the config
log) and roles are **Owner / Admin / Contributor / Viewer**.

### One engine, judged at each event's position {#authz-at-position}

Every event a device folds, and every event it authors, goes through the
same authorization engine, so authoring and folding can't disagree:

- **Roles as of the event.** Each chat event stamps the position of the
  workspace config log its author had seen. Proposal-set steps, votes and
  applies are judged against the roster **at that position**, never today's:
  demoting an Admin later doesn't undo their decisions, and promoting
  someone later doesn't validate steps they took before. An event whose
  position hasn't synced to this device yet **waits** (it is kept, left out
  of the chat, and blocks side effects on the proposal it touches) and is
  decided once it can be.
- **Members only.** Every author must be a workspace member at the event's
  position. Joining needs a single-use invite an Admin issued; a removed
  account also needs an Admin to re-add it.
- **Votes that mean something.** A vote counts only from the voter it
  names, at their role then (the role a vote claims is ignored), and only
  for the content digest it names; editing a proposal's patch voids earlier
  votes. Proposal status is derived from counted votes, and *Applied* comes
  only from a human member's apply event for the approved content. A vote
  signed by a `ligus serve` box enrolled as its owner's device doesn't count
  (see [Phone and serve approvals](../security/index.md#roles)).
- **Agents.** An agent can sign with its own key, certified by the host it
  is bound to and retired when it moves; such events are judged by a capped
  Agent tier that never votes, applies or administers.
- **Versioned rules.** The rules are versioned on the config log; a version
  this build doesn't know makes the workspace read-only on this device.
- **Bytes as received.** Envelopes are verified, stored and re-sent as the
  bytes received, so an event from a newer build keeps fields this build
  doesn't know and still verifies everywhere.
- **No environment.** Nothing that decides which events fold reads the
  environment; `LIGUS_IDENTITY_MODE` is ignored.

The workspace **founder is pinned via the invite**: the invite carries the
founding identity and a fingerprint of the founder's account key, a joining
device pins both, and projection only accepts the real founder's seed as the
base snapshot. So a malicious keyholder can't forge a competing base, or a
second key for the founder, to hijack or fork the workspace out from under
everyone else. Trust rosters are kept per workspace: a removal in one
workspace doesn't touch devices in another.

## The sandbox envelope around every agent turn

The workspace, not each runtime, decides how far an agent turn may reach, and
the same rules apply whether the turn runs on Claude Code, Codex, aider, pi or
an API model calling MCP tools:

- **Isolated worktree.** An agent that can edit files runs in its own git
  worktree, and its changes come back as a proposal for review. While the
  envelope is tightened (dearth mode, a chat lockdown), a turn that can't get
  a worktree is refused instead of running in place.
- **Filesystem allow-list.** Agents never write into `.git`, `.ligus`, `.hive`,
  `.claude`, `.vscode` or the workspace config files. Ligus's built-in file
  tools and the OS sandbox (below) both read this list from the envelope.
- **Egress policy.** Normally open (each runtime's own permission tier
  applies). Tightened, it is either *allow-listed MCP servers only* or
  *closed*. Ligus enforces it per runtime: Claude Code loses its web and shell
  tools and every MCP server outside the list; Codex runs with network access
  off; API models only see allowed MCP servers and never `run_command`.
  Runtimes Ligus can't confine from outside (aider, pi, Hermes, Antigravity,
  Copilot) are **refused** while egress is restricted, with a note in the chat.
- **Secret redaction.** API keys, tokens, private keys and Ligus invite codes
  in an agent's reply are masked before the reply is written to the log, so
  they never reach other members or the relay. Ligus's **own** secrets are
  masked by exact value as `[ligus-secret]`: the serve token, relay `hrt1`
  tokens, phone pairing credentials, device and workspace key seeds, and
  every key or token in this device's settings. That covers streamed
  replies and proposal titles, bodies and diffs.
- **No repository code run by Ligus.** Ligus's own git calls (worktrees,
  diffs, apply, the hygiene sweep) never run the repository's hooks or
  fsmonitor, and Claude Code loads no settings, hooks or plugins from the
  repository in any Ligus turn (nor your own, unless you
  [opt in](#claude-code-turn)).
- **An allow-listed environment.** Agent CLIs, MCP servers, `run_command`
  and pre-flight tests start with an empty environment plus an allow-list:
  `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TZ`, `TERM`,
  `TMPDIR`, the CA variables, `XDG_*` on Linux, the CLIs' own config-dir
  variables when you set them, the proxy variables Ligus sets, git
  attribution, and **one** provider key: the one you configured for that
  runtime in Ligus. A key in Ligus's own environment (`ANTHROPIC_API_KEY`,
  `OPENAI_API_KEY`, …) is no longer inherited; Ligus still uses it for its
  own API calls. Claude Code therefore uses its subscription login unless
  you give its runtime a key in Ligus.
- **No remote images in chat.** Markdown images from remote URLs render as
  links, so a reply can't send data to a server just by being displayed.

On Kubernetes the same envelope becomes the turn executor's pod spec and
NetworkPolicy.

## The OS sandbox around agent processes {#os-sandbox}

Every process Ligus starts for a turn (a CLI agent, a stdio MCP server,
`run_command`, a pre-flight test) runs inside an OS sandbox planned from the
envelope, the runtime and the author gate. The plan is the same on the
desktop, `ligus serve`, `ligus worker` and `ligus agent`.

On Linux this is the strongest of [three tiers](../ops/linux-sandbox.md):
bubblewrap + Landlock + seccomp where bubblewrap can create a user
namespace; Landlock + seccomp without namespaces where it can't (Ubuntu
24.04, whose AppArmor restricts user namespaces); none, with write-capable
turns refused, where neither is available. The table shows the first.

| | macOS (Seatbelt, `sandbox-exec`) | Linux (bubblewrap + Landlock + seccomp) |
|---|---|---|
| Writes | the turn's worktree (only when the turn may write), its scratch dir, the runtime's own state dirs (`~/.claude`, `~/.codex`, `~/.copilot`, …) | the same, as bind mounts on a read-only `/`; Landlock limits writes to them again |
| Never readable | Ligus's data dir (keys, `settings.json`, the database, `serve.token`), a `--token-file` elsewhere, `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config/gcloud`, `~/.kube`, `~/.netrc`, `~/.git-credentials` | the same (tmpfs over them), plus `$XDG_RUNTIME_DIR`, so the D-Bus session bus and Secret Service are out of reach |
| Read-only inside the worktree | `.git`, the protected dirs, `ligus.config.toml`, `hive.config.toml`, `.envrc`, matched by name at any depth and in any case (`sub/.claude/`, `.CLAUDE/`) | the same names at the top level (a missing one is pinned by an empty read-only directory); Linux file systems are case-sensitive, so `.CLAUDE/` is a different directory no CLI reads. Nested names are not pinned, but Ligus's own file tools and ACP `fs/*` refuse them |
| Network | denied except the turn's egress proxy on loopback | a fresh network namespace; the only way out is the egress proxy through a socket |
| Other | | seccomp denies `ptrace`, `process_vm_*`, `mount`, `bpf`, `keyctl`, kernel-module and `kexec` calls, new user namespaces, and new unix sockets (`socket(AF_UNIX)`), so `docker.sock`, an ssh-agent or a desktop service can't be reached; `socketpair` still works, and the Ligus MCP server is reached through a loopback forwarder inside the sandbox. Known host sockets (`docker.sock`, `podman.sock`, `containerd.sock`, `$SSH_AUTH_SOCK`) are also masked. `LIGUS_SANDBOX_UNIX_SOCKETS=allow` on the device lifts the unix-socket filter (the masks stay) for tools that need sockets of their own |

- **cwd and HOME.** An agent never runs in your home directory, in `/` or in
  Ligus's data dir; a turn without a worktree gets a fresh scratch dir
  (`$TMPDIR/ligus-turn-…`, mode 0700). Claude Code's own scratch
  (`CLAUDE_CODE_TMPDIR`) points there too, because it can't write
  `/tmp/claude-<uid>` inside the sandbox. The dir is removed when the turn
  ends or is cancelled; one a killed turn or a killed host left behind is
  swept at startup and every ten minutes, once no running process owns it
  and it is older than the longest turn (`LIGUS_TURN_TIMEOUT_SECS` plus ten
  minutes).
- **Egress proxy.** Each turn gets its own HTTP `CONNECT` proxy on
  `127.0.0.1`, with a random credential in the child's `HTTPS_PROXY`. It
  allows exactly the runtime's model endpoints (Claude Code:
  `api.anthropic.com`, `console.anthropic.com`, `claude.ai`; Codex:
  `api.openai.com`, `chatgpt.com`, `auth.openai.com`; Copilot:
  `api.githubcopilot.com`, `github.com`, `api.github.com`; Antigravity: the
  Google endpoints; an API runtime: its base URL) plus the hosts of the MCP
  servers this chat may reach. Under dearth mode only the allow-listed MCP
  servers' hosts are added; under a chat lockdown only the model endpoints
  remain. The `CONNECT` host and the TLS server name (SNI) must both match;
  there is no TLS interception. Every refused endpoint is recorded as a
  content-free decision and logged (host and port). A refused endpoint that
  may have mattered shows as a system note in the chat naming it; [known
  optional hosts](#refused-optional) are blocked quietly. A tool that ignores `HTTPS_PROXY`
  simply has no network. To let agents reach more hosts on your device under
  the normal envelope, switch on an [egress preset](#egress-presets) or set
  `LIGUS_SANDBOX_EGRESS_ALLOW`.
- **Keychain (macOS).** Subscription CLIs (Claude Code, Codex, Copilot,
  Antigravity) keep access to the login keychain, because their logins live
  there; every other agent process can't read the keychain files. Ligus's own
  keychain items are protected by their access lists, not by the profile.
- **Codex** runs its commands inside Ligus's sandbox instead of its own
  (Seatbelt and bubblewrap sandboxes can't nest).
- **No sandbox, no local agents.** Where no sandbox is available
  (Windows, a Linux host with neither bubblewrap and user namespaces nor
  Landlock 6.7+, `LIGUS_SANDBOX=off`), Ligus spawns nothing on a model's
  behalf: every CLI agent turn (read-only ones too), `run_command`, an
  approved proposal's command, a pre-flight test and a stdio MCP server is
  refused, with a note naming the platform and how to get a sandbox. An
  HTTP/API model still answers, as plain chat: it gets no built-in file
  tools, no `run_command` and no stdio MCP servers (the Ligus chat tools,
  which touch no local files, stay). `ligus doctor` says which tier a host
  uses and why.
- **The Landlock tier (Linux without user namespaces)** enforces the same
  reads, writes and egress allow-list without namespaces, with known gaps:
  TCP is limited by port rather than address, other processes are visible,
  and nothing can be created at the worktree's top level. See
  [The Linux agent sandbox](../ops/linux-sandbox.md), including the
  optional AppArmor profile that restores bubblewrap on Ubuntu 24.04.
- **Why the macOS profile is default-allow.** The Seatbelt profile allows
  by default and then denies what matters: every file write outside the
  turn's paths, reads of Ligus's data and your credential dirs, all network
  except the proxy and the Ligus MCP socket, LaunchServices, Apple Events and
  the pasteboard. A default-deny profile would also have to allow, by name,
  every Mach service, sysctl, IOKit class and file the agent CLIs and the
  tools they run need: `securityd` and `trustd` (the subscription logins and
  TLS), `opendirectoryd`, `notifyd`, `logd`, `cfprefsd`, POSIX shared memory
  for Node, ttys, and the toolchains agents call (`cargo`, `npm`, `python`,
  `go`, `git`). Those differ per CLI and per macOS release, and a missing one
  fails a turn in ways that look like CLI bugs. That is too much risk for
  2.0, so it stays a planned change. What default-allow leaves open:
  agents can look up other Mach/XPC services (one that acts on their behalf
  runs outside the profile), read files outside the denied dirs, and run any
  program, all as your user and still under the write, read and network
  rules above.
- **One control among several.** The sandbox does not replace the author
  gate, the MCP role gate, review of proposals or the human gate, and it is
  not a boundary against kernel bugs.

### What a plain Claude Code turn tries to reach {#refused-optional}

A Claude Code turn (2.1.x) with nothing switched on tries two hosts that
aren't on its allow-list. Both are blocked on purpose, and neither affects
the turn or the Ligus tools (checked live under Seatbelt, with and without a
bound code folder):

| Host | What it is | Matters? |
|---|---|---|
| `http-intake.logs.us5.datadoghq.com` | Claude Code's telemetry log intake | No. Telemetry stays blocked. |
| `mcp-proxy.anthropic.com` | the proxy behind your claude.ai MCP connectors (Drive, Gmail, ...) | Only if you want those connectors in Ligus turns: switch on the `claude-connectors` preset below. |

Ligus treats these, and other known telemetry and error-reporting hosts
(Datadog, Sentry, Statsig), as *known optional*: they're recorded and
logged but don't post a chat notice. When a turn is refused anything else,
the notice names it, says how to allow it, and lists the optional ones
apart as not affecting the turn.

### Egress presets {#egress-presets}

A preset is a named, opt-in set of hosts added to a chat's egress
allow-list. Every preset is **off by default**.

| Preset | Adds (port 443 only) | For |
|---|---|---|
| `package-registries` | npm: `registry.npmjs.org`; crates.io: `crates.io`, `static.crates.io`, `index.crates.io`; PyPI: `pypi.org`, `files.pythonhosted.org`; GitHub: `github.com`, `api.github.com`, `codeload.github.com`, `objects.githubusercontent.com`, `raw.githubusercontent.com` | agents, `run_command` and pre-flight tests that install dependencies (`npm install`, `cargo build`, `pip install`, git dependencies) |
| `claude-connectors` | `mcp-proxy.anthropic.com` | Claude Code turns only. Lets Claude Code use your claude.ai MCP connectors inside Ligus turns. **This lets the agent reach every service you connected there** (Google Drive, Gmail, and so on) with your access, so switch it on only for chats where you want that. Claude Code loads connectors only without its strict MCP mode, so with this preset on your own Claude Code MCP servers load too ([details](#claude-code-turn)). Claude Code's telemetry hosts stay blocked |

Where to switch them on:

- **Per workspace:** **Settings → Guards → Network presets**, the
  *This workspace* switch. Stored on this device as
  `egress_presets_by_workspace` in `settings.json`.
- **Per chat:** in the same panel, with that chat open behind Settings:
  *Workspace setting*, *On for this chat* or *Off for this chat*. A chat's
  own choice wins over the workspace's. Stored as `egress_presets_by_chat`.
- **For every turn on a host** (desktop, `ligus serve`, `ligus worker`,
  `ligus agent`): `LIGUS_SANDBOX_EGRESS_PRESETS=package-registries` (comma-
  separated names). This is how a headless worker or serve box opts in.

Presets are **device-local**: they change where agents on this device may
connect, so they are never synced, and neither a teammate nor a committed
config file can switch one on for you. Turns that run on another device
follow that device's settings.

A preset never loosens a guard. It applies only while the chat's egress is
open: under [dearth mode](#dearth-mode-a-tighter-entrance-on-demand) the
chat keeps only its allow-listed MCP servers, and under a chat
[lockdown](#self-medication-a-chat-that-shows-infection-locks-itself-down)
only the model endpoints. **Settings → Guards** shows *ignored* on the chat
while that is the case. Preset hosts go through the same proxy as
everything else: exact host and port match, and the TLS server name must
equal the `CONNECT` host.

### What a Claude Code turn can and can't do inside Ligus {#claude-code-turn}

Ligus runs Claude Code with your Claude login, but not with your personal
Claude Code setup. Claude Code has features that act outside a turn or
outside the machine: cloud routines that run on Anthropic's servers under
your account and connectors, scheduled tasks, background monitors, push
notifications, messages to your other Claude Code sessions, and your own
plugins, hooks, skills and MCP servers. A turn confined to its worktree and
egress allow-list could otherwise use them to start work that the sandbox
never sees. So every Claude Code turn Ligus starts (desktop, `ligus serve`,
`ligus worker`, `ligus agent`) gets only what the turn needs:

| Feature | In a Ligus turn | How |
|---|---|---|
| Read, edit, write, search files; in-process sub-agents | **Yes**, at the chat's permission tier | `--tools` allow-list. Sub-agents run in the same process, sandbox and tool set. |
| Shell (`Bash`) | At the *Edits + shell* tier and up, while egress is open | `--tools`; denied under dearth mode or a lockdown |
| Ligus's own tools (`mcp__ligus__*`) | **Yes**, pre-approved | `--allowedTools mcp__ligus`. Ligus authorizes each call itself. |
| The chat's MCP servers (Settings → MCP, filtered by the envelope) | **Yes**; pre-approved when the author may write | `--strict-mcp-config` with only those servers, `--allowedTools mcp__<id>` |
| Web search and web fetch | Only while the chat's egress is open | Web search runs on Anthropic's side, so the egress proxy never sees it; Ligus follows the chat's egress policy instead and removes both tools when egress is restricted. |
| Cloud routines and remote triggers (`RemoteTrigger`) | **No** | `--disallowedTools`, denied again in the turn home's settings |
| Scheduled tasks and wake-ups (`CronCreate`, `CronList`, `CronDelete`, `ScheduleWakeup`) | **No** | `--disallowedTools`, `CLAUDE_CODE_DISABLE_CRON=1` |
| Background tasks and monitors (`Monitor`, `TaskStop`, background shells) | **No** | `--disallowedTools`, `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` |
| Push notifications (`PushNotification`) | **No** | `--disallowedTools` |
| Messaging other Claude Code sessions (`SendMessage`, `ListAgents`) | **No** | `--disallowedTools`; the OS sandbox also refuses their local sockets |
| Multi-agent workflows (`Workflow`), design sync (`DesignSync`) | **No** | `--disallowedTools`, `CLAUDE_CODE_DISABLE_WORKFLOWS=1` |
| Worktree switching (`EnterWorktree`, `ExitWorktree`) | **No**: Ligus owns the turn's directory | `--disallowedTools` |
| Your MCP servers (`~/.claude.json`), the repo's `.mcp.json`, plugin MCP servers | **No** | `--strict-mcp-config` |
| claude.ai connectors | **No**, unless the chat turns on the `claude-connectors` [preset](#egress-presets) | `--strict-mcp-config`, `ENABLE_CLAUDEAI_MCP_SERVERS=false`, and the egress proxy refuses `mcp-proxy.anthropic.com` |
| Your settings, hooks, plugins, skills, slash commands | **No**, unless the workspace opts in (below) | `--setting-sources ""`, `disableAllHooks`, `--disable-slash-commands` |
| The repository's `.claude/settings*.json` (hooks, permission rules) | **No**, even with the opt-in | never a setting source |
| Any tool a newer Claude Code adds | **No** until Ligus lists it | `--tools` is an allow-list |

The tools and env above also hold for a `claude` the agent starts from its
own shell: the turn's copy of your user settings denies the same tools, and
the env variables are inherited. Your Claude login keeps working: Claude
Code reads it from your keychain (macOS) or its credentials file, not from
settings.

**Opting your own setup back in.** **Settings → Guards → Your own agent
setup**, *Use my Claude Code settings, plugins and skills in this
workspace*. With it on, that workspace's Claude Code turns load your user
settings (`--setting-sources user`; never the repository's), run your
hooks, and get your plugins and skills (your `~/.claude/plugins`, `skills`,
`agents` and `commands` folders are linked read-only into the turn's
home). Hooks and plugins then run inside the turn's OS sandbox, under its
write, read and egress rules. The opt-in is off by default, stored on this
device only (`cli_user_config_workspaces` in `settings.json`), and ignored
while a chat is locked down or the workspace is in dearth mode. It never
brings back the tools in the *No* rows above, and MCP stays strict (use
Settings → MCP to give a chat a server). The same switch lets Codex turns
load your `~/.codex/config.toml` (below). Turns on `ligus serve`,
`ligus worker` and `ligus agent` don't have the switch yet and always run
without your setup.

**What this does not stop.** At the *Edits + shell* tier and above the
agent runs commands as you, inside the sandbox, and your Claude login is
reachable from there (it has to be, for Claude Code to run). A command that
starts a fresh `claude` with its own flags (for example
`claude -p --setting-sources ""`) gets Claude Code's full tool set back,
cloud routines included, and the egress proxy can't tell those calls from
model calls because both go to `api.anthropic.com`. Cron, background tasks
and workflows stay off through the inherited env. Treat the shell tiers as
handing the agent your Claude account, and use the read-only or *Accept
edits* tier for chats whose input you don't trust.

**The other subscription CLIs.**

| Runtime | What Ligus does | Still loads |
|---|---|---|
| Codex (`codex exec`) | `--ignore-user-config` unless the workspace opted in: your `config.toml` MCP servers, `notify` program and web search settings stay out; the sign-in still loads. Under restricted egress also `network_access=false` and no MCP servers. | Codex over ACP (`codex-acp`) is not covered yet. |
| Copilot (`copilot -p`, `copilot --acp`) | `--disable-builtin-mcps` (the built-in GitHub MCP server acts on GitHub with your sign-in), `--allow-tool=ligus` so Ligus's tools aren't refused in `-p` mode | your `~/.copilot/mcp-config.json` servers, plugins and skills (Copilot has no strict MCP switch). Refused while egress is restricted. |
| Antigravity (`agy`) | `--disable-slash-commands` | your MCP servers and plugins (no strict switch). Refused while egress is restricted. |
| Any CLI over ACP | Ligus's own tool calls are pre-approved; every other permission request follows the chat's tier | |

## Sandbox defaults {#sandbox-defaults}

These defaults are deliberate and stay on in 2.0.

**Agents can't commit inside a turn.** `.git` is read-only to every agent
process, at every tier and on every host. An agent edits files in its
worktree; Ligus captures the diff as a proposal, and the commit happens
when the proposal is applied.
*Why:* a commit is history a reviewer didn't approve, and write access to
`.git` would also let an agent plant hooks or rewrite config that runs
later, outside the sandbox. Keeping every change as a reviewable diff is
what makes the review gate and the human gate mean something.

**No OS sandbox, no local agents.** Where Ligus can't confine an agent
process (`LIGUS_SANDBOX=off`, Windows, Linux without bubblewrap and user
namespaces or Landlock), every CLI agent turn is refused before it starts,
at any tier, with a note in the chat that names the platform and the fix:
on Linux, install bubblewrap with unprivileged user namespaces enabled, or
run Linux 6.7+ for the Landlock tier; on macOS, Seatbelt is built in (check
`LIGUS_SANDBOX` isn't `off`). **Windows isn't supported for agent turns**
that run local programs: run CLI agents on a macOS or Linux host (a `ligus
worker` or `ligus serve` box) and chat from Windows. Commands
(`run_command`, an approved proposal's command), pre-flight tests and stdio
MCP servers are refused the same way. HTTP/API models still answer, without
local tools.
*Why:* without the sandbox nothing stops a CLI from reading everything your
user account can: Ligus's database (`hive.db` isn't encrypted at rest), its
keys and tokens, your SSH keys. A read-only tier only stops writes, so
refusing write-capable turns alone left every read-only turn able to read
the data dir. Refusing is safer than running the agent with your full user
rights and hoping its own permission prompts hold.

**On workers, always-writing CLIs need an Admin author.** Codex, aider, pi
and Hermes have no read-only tier, so on a worker (and on every other host)
they run only for a message whose author was an Admin or Owner when they
posted it; for anyone below, the turn is refused with a note
([the author gate](#author-gate)). `LIGUS_MCP_MIN_ROLE` can lower the floor;
Ligus warns at startup when it does.
*Why:* a worker runs turns on its owner's keys, subscriptions and machine
for whoever writes in the chat. A Contributor's message shouldn't be able
to drive a tool that always edits files and runs commands there; Claude
Code, Copilot and Antigravity drop to read-only instead, but these CLIs
can't.

## Who can drive a turn: the author gate {#author-gate}

A turn runs on the host's keys and files on behalf of whoever wrote the
message that triggered it. Every host (desktop, `ligus serve`, `ligus
worker`, `ligus agent`) applies the same gate to every runtime:

- The floor is **Admin** for a turn that can write (a raised permission tier,
  or a CLI that always edits: Codex, aider, pi, Hermes) and **Contributor**
  for a read-only one. `LIGUS_MCP_MIN_ROLE` overrides it; a value below the
  default logs a warning at startup.
- At or above the floor the turn runs as configured. Below it, an API model
  gets no tools, Claude Code, Antigravity and Copilot run at their read-only
  tier (and the OS sandbox mounts the worktree read-only), and Codex, aider,
  pi and Hermes are refused with a note in the chat. An author that can't be
  resolved is below every floor. On the desktop, your own messages always
  clear it (your own messages, not an agent's reply your device signed,
  and not a task the board posts for you by automatic pickup; see below).
- The author's role is the one they held **when they posted the message**
  (the roster at the message's position), so a promotion after the fact
  doesn't widen an old message's reach.
- **An agent never lends a turn authority.** When the trigger is an
  agent's message (a hand-off, an @mention cascade, the primary agent
  mentioning another), it has no role of its own: Ligus follows the
  message's reply link back through every agent hop to the human who
  started the chain, and the turn runs at the **lowest** role of the humans
  behind it. This holds whichever key signed the agent's message (the
  agent's own key, or the host's when the agent has none: the device-local
  workspace, a host without an agent vault, a key that couldn't be
  certified). A peer device's agent message is also capped at that device's
  own member's role, so pointing a reply link at an Owner's message borrows
  nothing. A chain with no human behind it runs read-only.
- **Workflow stages run at the initiator's role.** A stage prompt is posted
  by whichever device drives the run (after a restart or a lease takeover
  that can be another member's device), so it is judged at the lower of the
  poster's role and the role of the member who started the run (read from
  the signed record that created it). The approver of the workflow
  definition isn't part of it: approval reviews what the stages say, and
  the person who starts a run is the one whose authority it uses.
- **The stamped member must be the signer.** A message's actor stamp names
  a member, and a member's actor id is their account id. The gate counts
  that member's role only when the stamp names their account and the
  device that signed the event is certified for that account and not
  revoked at the stamped position; anything else gets no role. Ingest
  enforces the same binding first: an event whose stamp names a known
  account (by its account field or by its actor id) that the signing device
  doesn't sign for is quarantined as impersonation on every device, so a
  Contributor can't post as the Owner or promote themselves with a
  `MemberRoleChanged` stamped with the Owner's id.
- **One workspace per chain.** Every event on the chain is judged against
  the chat's own workspace. An event in the chat's log stamped with another
  workspace (even one where its author is Owner and this device also holds
  the config log) makes the chain resolve to no role, and so does a workflow
  run whose first record names another workspace.
- **Automatic board pickup has no author.** With auto pickup on, the task
  the board posts (a proposal's or a failed stage's title, written by
  whoever filed it) is posted as the runtime's effect, not as you: it has no
  human behind it and runs below every floor. Picking an item up yourself
  still posts it as your message, at your role.
- One shared function (`ChatService::trigger_author_role`) decides this on
  the desktop, `ligus worker`, `ligus serve` and `ligus agent`.

## Trust this workspace {#workspace-trust}

A repository can ship runtimes and MCP servers (`ligus.config.toml`,
`.ligus/runtimes.json`, `.ligus/mcp-servers.json`): commands Ligus would run
and endpoints it would send prompts to. They load **inert**. Ligus shows
*"This workspace wants to run N commands (MCP servers, runtimes)"*; **Review**
lists each exact command line and the file it came from, and you enable
items one by one or **Trust this workspace** for all current ones.

- The decision lives only on this device (a table in its database), keyed by
  the repository (its path and first commit) and a digest of each item's
  command, arguments and URL. A changed command is a new item and is inert
  again.
- Items you add in Settings are trusted as you save them.
- Headless hosts use `ligus trust list | enable <digest> | enable-all`.
- The pre-flight test command comes only from `LIGUS_PREFLIGHT_TEST` on your
  device, never from the repository's git config.

## Telemetry stays on your device {#facts}

Per-turn measurements (which agent answered, task class, tool calls, test
results, waits, touched-file keys, route stops) are **facts** on the device
that ran the turn, not events in the signed log. Each host syncs one bounded
summary per agent per day (turn, error and test counts per task class,
proposal outcomes), which is what other devices score from. Routing
decisions stay in the log, because other devices follow them. External
observability tools read facts with `ligus facts export --jsonl` (schema
`ligus.facts.v1`: ids, enums, numbers and timestamps only; any string that
isn't an id or looks like a secret is exported as `redacted`).

The export is the one supported way for another tool to watch Ligus: it
never needs to open the database or decrypt the log. Each line is
`{"schema","seq","timeUnixMs","kind","attributes"}`, and it carries:

- **Measurements** (`turn`, `touch`, `progress`, `edgeWait`, `routeStop`,
  `toolCall`): the facts above, from this device only.
- **Guard signals** (`guardSignal`): this device's lease heartbeats and
  receipts, boundary decisions, tool selection, hygiene catches, worktree
  states and throttles, one `ligus.signal.<field>` attribute per field.
- **State facts**, observed from this device's projection of the signed log
  for every chat whose log changed since the last export: `reply` (agent and
  assistant replies: ids, role, kind, runtime, trigger, claim, streaming,
  tool-call count, output size in bytes), `proposal` (kind, the status
  Ligus's fold derived, vote, change-request and comment counts, diff line
  and file counts, version chain), `workflowRun` and `workflowStage` (status,
  attempts, dependencies by node id), `agent` and `host` (the roster, from
  the workspace config log) and `lease`. `timeUnixMs` is when the export
  looked; `seq` is the event-log row it looked at, shared by every state
  line of one export.

Titles, bodies, names, prompts, diffs and paths never appear. Device ids
are exported as a short hash; workspaces as `ligus.workspace.id_hash`. The
last line has kind `cursor`; pass its `ligus.cursor` value as the next
`--since` to read only what is new (`--since` also takes unix milliseconds,
which observes every chat again). Readers should reject lines whose
`schema` they don't know and ignore kinds they don't.

## Dearth mode: a tighter entrance on demand

An Admin can put the workspace in [dearth mode](../features/guards.md#dearth-mode)
when it is under pressure. New members then need two different Admins, a
newly certified device needs an Admin of another account to approve it before
its certificate counts, agents keep only allow-listed MCP servers, and changes
to protected paths need a human member's own approval. The switch, the join
approvals and the votes that satisfy a human gate are all re-checked on
ingest: a vote counts toward a human gate only when the voter it names is the
person who signed it, and an approval carried inside a proposal snapshot never
counts.

While dearth mode is on, every device holds, in every identity mode, the
events of a device still waiting for its second approval and any event no
certified device vouches for. Held events aren't dropped: sync waits on
them and judges them again, so they apply once the device is approved. The
protected-path gate
reads the paths in the proposal's diff itself, not the list the author
declared, and matches them case-insensitively; a human approval stops
counting if the patch changes afterwards.

Residual: outside dearth mode, the default identity mode still accepts events
from a device with no certificate.

## Self-medication: a chat that shows infection locks itself down

When an agent turn reads a prompt-injection marker in a tool result, adds a
credential to a diff, or changes CI configuration nobody asked about, Ligus
[locks that chat down](../features/guards.md#chat-lockdown): no network or
MCP access for its agents and a human approval on every proposal, until a
human member clears it. The lock is an event on the workspace config log, so
every device applies it; clearing it is re-checked on ingest (a non-member,
Viewer or agent account can't).

## What this page does not cover

The full [security model](../security/index.md), the
[threat model and known limitations](../security/threat-model.md), and the
[incident runbooks](../security/runbooks.md) have their own section.

Private key seeds live in the OS secret store: on macOS a dedicated
keychain file inside the data dir, on a Linux desktop the Secret Service
(through `secret-tool`), on a headless Linux box `systemd-creds`; owner-only
files are a stated fallback only where no store works. See
[Identity & devices](identity.md#key-storage) for what protects them and
[Self-hosting a relay](../networking/self-host.md) for connection-level access
control.

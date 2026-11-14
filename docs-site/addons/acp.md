# Subscription CLIs over ACP

Ligus drives the subscription coding CLIs through the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP) when it can,
and through each CLI's headless mode when it can't. ACP is a JSON-RPC
protocol on the CLI's stdin and stdout. Over ACP, Ligus:

- sees the agent's text, its reasoning, its tool calls and its plan as they
  happen;
- answers the agent's **permission requests**, by your permission tier or by
  asking a person in the chat;
- serves the agent's file reads and writes itself, inside the turn's
  worktree only;
- hands the agent the [Ligus tools](ligus-mcp.md) as an MCP server.

The headless adapters (`claude -p`, `codex exec`, `agy --print`,
`copilot -p`) stay as the fallback.

## What runs over ACP {#matrix}

| Runtime | ACP entry point | Pinned | Notes |
|---|---|---|---|
| GitHub Copilot CLI | `copilot --acp` (built in) | `@github/copilot` 1.0.112 | uses your `copilot login` |
| Google Antigravity | `agy acp` (built in) | Antigravity CLI 1.5.0 or later | uses your Google sign-in |
| Codex | `codex-acp` (adapter) | `@agentclientprotocol/codex-acp` 2.6.0 | install the adapter yourself; Ligus never downloads it during a turn |
| Claude Code | `claude-agent-acp` (adapter) | `@agentclientprotocol/claude-agent-acp` 0.107.0 | **only with an API key configured in Ligus**; see below |

The pins were checked against the ACP registry
(`cdn.agentclientprotocol.com/registry/v1/latest/registry.json`) on
2026-11-14. Install the adapters with:

```sh
npm install -g @agentclientprotocol/codex-acp@2.6.0
npm install -g @agentclientprotocol/claude-agent-acp@0.107.0
```

!!! note "Claude Code and your claude.ai login"
    Anthropic's Agent SDK terms don't allow third-party products to offer the
    claude.ai login through the SDK without approval. So a Claude Code runtime
    goes over ACP only when you've set an Anthropic API key for it in Ligus.
    With your Pro or Max login it keeps using `claude -p`, which runs Claude
    Code itself. Ligus also keeps the headless adapter for Claude when a chat
    is in dearth mode or locked down, because the adapter can't yet be held to
    the same restricted settings (user settings only, hooks off, only the
    allowed MCP servers).

## Choosing the driver {#driver}

Each runtime has a `driver` setting: `auto` (the default), `acp` or
`legacy`. In `ligus.config.toml`:

```toml
[[runtimes]]
id = "copilot"
provider = "copilot"
driver = "acp"     # auto | acp | legacy
```

- `auto` uses ACP when the entry point is installed, and falls back to the
  headless adapter for that turn if the agent doesn't answer ACP's
  `initialize`. A fallback is recorded as a device-local fact.
- `acp` always tries ACP first, with the same one-time fallback.
- `legacy` always uses the headless adapter.

## Permissions {#permissions}

An ACP agent asks before it edits a file, runs a command or fetches a page.
Ligus answers from your permission tier for CLI agents (Settings, Agents):

| The agent wants to | Read-only (default) | Edits | Edits + commands | Bypass |
|---|---|---|---|---|
| read or search | allowed | allowed | allowed | allowed |
| edit, delete, move | **asks a person** | allowed | allowed | allowed |
| run a command | **asks a person** | **asks a person** | allowed | allowed |
| fetch from the web | **asks a person** | **asks a person** | **asks a person** | allowed |
| anything else | **asks a person** | **asks a person** | **asks a person** | **asks a person** |

- "Asks a person" posts the request in the chat as a question (the same one
  `ligus_ask_human` uses). Because allowing it lets the agent act on this
  host, only **the host's own person or the agent's owner** can grant it,
  from the desktop or the phone. If anyone else answers first, the request
  is denied. The turn waits, and its idle timer is paused while it waits.
  With no answer within 10 minutes (`LIGUS_ACP_ASK_TIMEOUT_SECS`), the
  request is denied.
- When the message's author is below this host's tool floor (H1), edits and
  commands are **denied** outright. A person can't widen the gate from a
  permission prompt.
- Network fetches are denied in dearth mode and in a lockdown.

The agent's `fs/write_text_file` requests are written by Ligus only inside
the turn's worktree, never into `.git`, the protected dirs,
`ligus.config.toml` or `.envrc` (matched by name at any depth and ignoring
case), never through a symbolic link, and only when the tier allows edits.
`fs/read_text_file` serves regular files of at most 16 MiB.

## The CLI's own config {#cli-home}

Every CLI agent, on either driver, runs with a **per-turn home**. Ligus
points `HOME`, the XDG directories and the CLI's config-dir variable at a
fresh directory inside the turn's scratch dir, and copies in only what the
CLI needs to start and stay signed in: the top-level files of its config
directories (`~/.claude`, `~/.codex`, `~/.copilot`, `~/.gemini` and so on).
Hook scripts and history are not copied.

So nothing an agent writes into its CLI's config (a hook, an MCP server)
outlives the turn, or runs the next time you start the CLI yourself. Your
real config directories aren't writable from the sandbox. If the CLI
refreshes its sign-in token during the turn, Ligus copies the refreshed
credential file back afterwards. It does that only for the CLI's credential
files, only when the result is valid JSON, and never through a symbolic or
hard link the agent left in its turn home.

On macOS the login keychain stays reachable, so subscription sign-ins keep
working. If a CLI misbehaves with a per-turn home, `LIGUS_CLI_HOME=shared`
(on that device) restores the old behavior. Please also report it.

## Troubleshooting {#troubleshooting}

- **"ACP initialize failed" in the log, then a normal reply.** Your CLI is
  older than its ACP entry point, so the turn fell back to the headless
  adapter. Update the CLI, or set `driver = "legacy"`.
- **A Claude Code runtime never uses ACP.** That's expected without an API
  key; see the note above.
- **The turn waits on "asks permission to run …".** Answer it in the chat or
  on the phone, or raise the CLI permission tier.

# `ligus` — headless Ligus client

The binary a cloud-hosted agent (or a script) runs to participate in a Ligus
workspace **without the desktop app**. It runs the same runtime the app does
(`ligus-core` + `ligus-runtime`, no Tauri): create-or-load a signing identity,
connect to a relay, sync the workspace's end-to-end-encrypted event log, and
read/write **signed** events. This is the "worker" client from the design
spec §12 (D21–D23).

## Build / install

```sh
brew install langstroth-ai/ligus/ligus   # macOS + Linux; tracks releases
# or the one-line installer (prebuilt binary onto your PATH):
curl -fsSL https://raw.githubusercontent.com/langstroth-ai/ligus/main/install.sh | sh
# or from source:
cargo build --release -p ligus-cli         # → target/release/ligus
cargo run -p ligus-cli -- <command>        # run in place
```

## Configuration (environment)

Everything is env-driven so it drops into a container or a secret mount. A
**persistent data dir** keeps a stable identity across restarts.

| Variable | Purpose |
| --- | --- |
| `LIGUS_DATA_DIR` | Where identity + `ligus.db` live. Default: `$HOME/.ligus` (or an existing `$HOME/.ligus`). **Must persist** for a stable identity. |
| `LIGUS_RELAY_URL` | Relay base URL. Required for `sync`, `send --push`, `agent`. |
| `LIGUS_RELAY_ACCESS_TOKEN` | Relay access token (`hrt1…`/hex). **Provision out-of-band** — there is no headless self-service. |
| `LIGUS_WORKSPACE` | Relay room / workspace name. Default: `default`. |
| `LIGUS_WORKSPACE_KEY` | E2EE passphrase. Omit only if the relay is plaintext. **Provision out-of-band.** |
| `LIGUS_PROVIDER` | For `agent`: `anthropic` (default) \| `openai` \| `openrouter` \| `ollama`. |
| `LIGUS_MODEL` | For `agent`: model id (e.g. `claude-sonnet-4-5`, `gpt-5`). |
| `LIGUS_PROVIDER_API_KEY` | For `agent`: the model key (or provider-specific `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `OPENROUTER_API_KEY`). |
| `LIGUS_MCP_CONFIG` | Optional path to an MCP-servers TOML that gives `agent`/`worker` tools (Linear/GitHub boards). Unset → no tools. See [MCP tools](#mcp-tools-for-headless-agents). |
| `LIGUS_MCP_MIN_ROLE` | Minimum **triggering author** role before MCP tools run: `viewer`\|`contributor`\|`admin`\|`owner`. Default `admin` when the turn can write (a raised `LIGUS_WORKER_PERMISSION_MODE`, or codex/aider/pi/hermes), `contributor` when read-only; a lower explicit value logs a warning. See [Requester-role gate](#requester-role-gate-ligus_mcp_min_role). |

## Commands

```
ligus enroll <hivews1:invite>       # one-shot: decode the invite (relay/room/key) → save config.toml
       [--token <hrt1>]            #   …and set the relay identity token (membership-enforcing relays)
ligus join <invite-code>            # redeem a relay invite code → self-enroll this box at its role
ligus setup [--apply]               # detect providers on this box; --apply defines keyless runtimes
ligus config <export|import <file>> # round-trip the workspace runtime set as TOML
ligus whoami                        # this client's identity (account/device ids); bootstraps keys on first run
ligus chats                         # list chats in the local store
ligus new [title]                   # create a chat in the workspace → prints its id
ligus tail <chat-id> [--watch]      # print a chat's transcript; --watch syncs + follows
ligus send <chat-id> <message…>     # post a signed message
       [--push]                    #   …and sync it out to the relay
ligus sync [--watch]                # one sync round (push everything unpushed, then fetch+ingest)
ligus runtimes                      # list workspace-owned runtimes (spec §12.5)
ligus add-runtime <id> <name> <provider> <model>
       [--endpoint <url>] [--secret-ref <name> | --secret <value>]
ligus rm-runtime <id>
ligus agents                        # list workspace agents
ligus add-agent <name> <runtime-id> [--host <id>] [--role <role>]
ligus hosts                         # list workspace hosts (devices + workers)
ligus register-worker [--label <name>]      # register this box as a worker host
ligus set-agent-host <agent> <host-id>      # bind an agent to a host
ligus worker [--label <name>]       # run the worker daemon (spec §12.4)
ligus queue                         # show queued work (unanswered mentions + host status)
ligus agent [name] [--runtime <id>] # act as the primary responder (see below)
```

Chat ids come from `ligus chats` (or `ligus sync` then `ligus chats` to pull a
workspace the app created).

> **Where should the agent run?** See
> [Headless agents & setups](../../docs-site/docs/operate/concepts/headless-agents.md) for the
> four topologies (shared data dir · local relay · remote worker · team) and how
> to pick one.

## Production daemon: `ligus worker`

**`ligus worker` is the production way to run agents headless.** It registers the
box as a workspace host, drains queued `@mentions` for the agents assigned to it,
and runs them on **workspace-owned** runtime credentials (§12.5) — never a
personal key. It:

- **claims each turn** before answering, so a worker and a desktop (or another
  worker) that both own an agent never double-reply the same mention;
- **survives transient failures** — a store/relay error is logged and retried
  (with exponential backoff on sync), not fatal to the daemon;
- **continues agent→agent cascades** across devices, up to a generous per-turn
  ceiling (a runaway/loop backstop, not a conversation limit), so 2+ agents can
  hold a real multi-turn thread with no human in the loop;
- **gates MCP tools** on the triggering author's role (`LIGUS_MCP_MIN_ROLE`).

```sh
export LIGUS_DATA_DIR=/var/lib/ligus LIGUS_RELAY_URL=https://relay.example \
       LIGUS_RELAY_ACCESS_TOKEN=hrt1… LIGUS_WORKSPACE=acme LIGUS_WORKSPACE_KEY=…
ligus register-worker --label prod-box     # once
ligus set-agent-host acme-bot <host-id>     # assign agents (or from the desktop)
ligus worker --label prod-box               # run the daemon
```

## `ligus agent` — local dev only

`ligus agent [name]` is a foreground loop that replies — as the workspace's
**primary responder** — to any new un-mentioned or `@primary` user turn, using a
**personal** provider credential. Because a personal key is *not* workspace-owned
(§12.5), it is a **local-dev convenience only** and refuses to start unless you
opt in with `LIGUS_ALLOW_PERSONAL_AGENT=1`. For anything unattended or shared, use
`ligus worker` above.

```sh
export LIGUS_DATA_DIR=/var/lib/ligus LIGUS_RELAY_URL=https://relay.example \
       LIGUS_RELAY_ACCESS_TOKEN=hrt1… LIGUS_WORKSPACE=acme LIGUS_WORKSPACE_KEY=… \
       LIGUS_PROVIDER=anthropic LIGUS_MODEL=claude-sonnet-4-5 ANTHROPIC_API_KEY=sk-… \
       LIGUS_ALLOW_PERSONAL_AGENT=1
ligus agent acme-bot
```

### Rolling your own loop

If you want custom behaviour (only answer certain chats, run tools, decide when
to speak), skip `ligus agent` and script the primitives:

1. `ligus sync` — pull new events.
2. `ligus chats` — see chats; `ligus tail <id>` — read a transcript.
3. Decide whether/what to reply.
4. `ligus send <id> "<reply>" --push` — post it and sync out.
5. Sleep briefly; repeat from 1.

Each `send` is a signed event; the next push syncs it to every member.

### Agent operating instructions (copy into a system prompt)

> You participate in a Ligus workspace through the `ligus` CLI (already configured
> via environment variables). To read the conversation, run `ligus sync` then
> `ligus tail <chat-id>`; list chats with `ligus chats`. To reply, run
> `ligus send <chat-id> "<your message>" --push`. Only reply when a message is
> addressed to you (`@primary` or an un-directed question). Keep replies concise.
> Re-run `ligus sync` before reading so you see the latest. Never invent chat ids —
> get them from `ligus chats`.

## MCP tools for headless agents

`ligus agent` and `ligus worker` can call **MCP tools** — so a hosted agent reaches
external boards like **Linear** or **GitHub** — when you point `LIGUS_MCP_CONFIG`
at a TOML file listing MCP servers. Unset (or a missing file) ⇒ no tools ⇒ the
agent behaves exactly as before (a single streamed completion). When the config
yields ≥1 tool and the runtime is **Anthropic** with a key, replies run through
the agentic tool loop (model ↔ tool_use ↔ tool_result, mirroring the app).

```sh
export LIGUS_MCP_CONFIG=/etc/ligus/mcp.toml
# provide each server's token out-of-band (never in the TOML):
export LIGUS_MCP_LINEAR_TOKEN=lin_…   GITHUB_PERSONAL_ACCESS_TOKEN=ghp_…
ligus worker --label prod-box
```

The file reuses the app's `mcp_servers` table shape:

```toml
# Remote HTTP/SSE server — bearer token resolved from `token_env` (env, not file).
[[mcp_servers]]
id = "linear"
transport = "http"                     # "http" | "sse" | "stdio"
url = "https://mcp.linear.app/mcp"
token_env = "LIGUS_MCP_LINEAR_TOKEN"    # NAME of the env var holding the bearer

# Local stdio server — the spawned child inherits the worker's environment,
# so its provider token is provisioned there (e.g. GITHUB_PERSONAL_ACCESS_TOKEN).
[[mcp_servers]]
id = "github"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
# enabled = false                      # servers default to enabled here; opt out per-server
```

- **The file is the opt-in.** Unlike the app (which gates each server behind a UI
  toggle), a headless server defaults to `enabled = true` — you wrote and
  provisioned the file. Set `enabled = false` to keep one inert.
- **Secrets never live in the TOML.** An http/sse server's bearer is named by
  `token_env` and read from the worker's environment (same out-of-band convention
  as `LIGUS_WS_SECRET_*`); a stdio server's child inherits the worker env.
- **Anthropic only** (as in the app) and only when a key resolves; other
  providers / no key fall back to the plain streamed reply.

### Requester-role gate (`LIGUS_MCP_MIN_ROLE`)

MCP tools run on the **worker's** provider key and MCP servers, so a message that
triggers a tool call spends the worker owner's credentials and reaches their
boards. To stop a low-privilege member from driving those tools, the tool loop is
**gated on the triggering message author's workspace role**. Before offering tools
for a reply, the worker resolves the author of the user message it is answering
(via the message's `actor_identity`) and looks up their role in the chat's synced
member roster. Tools are executed **only** when that role is **≥ `LIGUS_MCP_MIN_ROLE`**
(`viewer` | `contributor` | `admin` | `owner`, case-insensitive; default
**`admin`** for a write-capable turn and **`contributor`** for a read-only one). Otherwise the agent still replies, but through the **tool-free**
streamed path — the under-privileged requester simply isn't offered tools — and a
line is logged to stderr noting the withheld message id and roles.

- **Default:** a workspace **Viewer** can never make the worker execute tools;
  a **Contributor** can drive a read-only turn and an **Admin** a write-capable
  one. Set `LIGUS_MCP_MIN_ROLE` to override; unknown/absent ⇒ the tier default.
  The same gate runs on `ligus agent` and the desktop, and every agent process
  runs in the OS sandbox (see the docs' Security & trust page).
- **Fail closed:** if the author can't be resolved (no `actor_identity`), tools are
  withheld.
- This gate is independent of the §12.5 workspace-credential enforcement and does
  not change the non-MCP reply path or reply posting.

## Workspace-owned runtimes (spec §12.5)

A **detached** agent — one that keeps working when its owner's laptop is closed —
must run on a **workspace-owned credential**, never a member's personal API key.
A *workspace runtime* is that: a provider/model the workspace owns, synced to
every member and worker via the config log, with a credential the workspace
provides. Its raw key does **not** have to ride the synced log.

Define one (owner/admin), then run an agent on it:

```sh
# key stays OFF the synced log — the worker holds it out-of-band:
ligus add-runtime ws-claude "Team Claude" anthropic claude-sonnet-4-5 --secret-ref acme
ligus sync                                   # share it with the workspace
# on the worker, provision the workspace secret and run the agent on it:
LIGUS_WS_SECRET_acme=sk-… ligus agent teambot --runtime ws-claude
```

- `--secret-ref <name>` — the worker resolves the key from env
  `LIGUS_WS_SECRET_<name>`. Raw keys never touch the synced stream. `ligus runtimes`
  shows `✓` when the secret is present on this box, `(missing)` otherwise.
- `--secret <value>` — carries the key **E2EE on the config log**. Zero worker
  provisioning (the workspace key already unlocks it); the key does land on every
  member's disk, so use it only for trusted workspaces.
- neither — a keyless runtime (e.g. local Ollama).

`ligus agent --runtime <id>` resolves provider/model/credential from the workspace
runtime; without `--runtime` it falls back to a **personal** env key
(`LIGUS_PROVIDER`/`LIGUS_MODEL` + key).

## The worker daemon (spec §12.4)

A **host** is a machine that executes an agent's turns — a member's `device` or an
always-on `worker` the workspace owns. An agent binds to a host; an agent bound to
a **worker** is *detached* — it keeps running when its owner's laptop is closed.

`ligus worker` is that always-on process. It registers this box as a worker host
(its host id **is** the box's device id — no extra provisioning), then continuously
hosts the agents bound to it: when a hosted agent is `@mentioned`, it replies via
that agent's **workspace runtime** (§12.5) and posts the reply. A failing turn is
isolated and logged; the loop keeps running.

Full setup on a worker box:

```sh
# once, to define the shared roster (syncs to everyone):
ligus add-runtime ws-claude "Team Claude" anthropic claude-sonnet-4-5 --secret-ref acme
ligus register-worker --label prod-box            # → prints this box's host id
ligus add-agent reviewer ws-claude --host <host-id> --role "Reviews diffs"
ligus sync

# then run the daemon (with the workspace secret provisioned):
LIGUS_WS_SECRET_acme=sk-…  ligus worker --label prod-box
```

Now `@reviewer` in any chat is answered by this worker, on the workspace credential
— even while every human is offline. `ligus hosts` / `ligus agents` show the roster
and bindings; `ligus set-agent-host <agent> <host>` re-binds.

**Enforced:** a detached agent must run on a *workspace* runtime. If a hosted
agent isn't bound to one (or its secret isn't on the box), the worker logs a skip
rather than falling back to a personal key.

### Queued work & the offline handoff (spec §12.5)

An unanswered `@mention` of an agent is **queued work** — it waits for that
agent's host. `ligus queue` lists it and shows whether each agent's host is online:

```
@reviewer   in <chat>  [prod-box · online]
@bob-claude in <chat>  [Bob's Mac · OFFLINE → queued]
@scout      in <chat>  [owner-device]
```

The worker **drains** the queue: it answers every *unanswered* mention for the
agents bound to it — backlog included — so it catches up on work addressed while
it was **down**, and picks up an agent the instant it's **reassigned** to it. It
heartbeats its presence so hosts show online/offline, and won't tight-loop retry
a hard-failed turn.

**Run on worker instead.** When an agent's own host is offline, move it to a
running worker to drain its queue:

```sh
ligus set-agent-host bob-claude <worker-host-id>   # reassign; the worker answers it next tick
```

## Limits & security (spec §12)

- **Credentials are provisioned out-of-band.** The relay token and workspace
  key must be handed to the box (env/secret); there's no headless self-service
  for either.
- **Personal vs workspace credential.** Without `--runtime`, `ligus agent` runs on
  a *personal* env key. With `--runtime <id>` — and always inside `ligus worker` —
  it runs on a **workspace-owned** runtime (§12.5); the worker daemon *enforces*
  that a detached agent uses one. The **queue/offline handoff** and host presence
  are implemented (above). What's still CLI-only is the *app-side* surfacing —
  showing queued work and a one-click "run on worker instead" in the desktop UI.
- **Agents never touch another machine's filesystem** (spec §12.6): what crosses
  the relay is chat and, eventually, diffs — not remote file writes.

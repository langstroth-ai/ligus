# Agents (built-in vs BYO)

Ligus's agent model is intentionally thin. The app isn't an agent
itself — it hosts agents you bring.

## WorkspaceAgent — the user-facing concept

A `WorkspaceAgent` has three fields:

- `name` — what peers `@mention` (e.g. `@coder`).
- `runtimeID` — which configured runtime drives it.
- `ownerActorID` — whose device the runtime lives on (`""` = shared).

Its owner also decides who may use it and sets its daily budgets (`access`,
`limits`, `notifyOnUse`; see
[Who can use an agent, and who pays](#who-can-use-an-agent)).

That's it. The agent isn't a "persona" with hardcoded prompts. It's
a pointer: "when someone in this chat says `@coder`, route the
message to runtime `aider-runtime` on Alice's device."

Configure agents from the right-rail **Tools** pane → **Workspace** → **Agent roster**.

<!-- Screenshot pending (Workspace Agents pane): images/agents-pane.png. See images/CAPTURE_GUIDE.md; restore the image line once it is captured. -->
## Why no built-in personas?

Two reasons:

1. **They would duplicate work that aider, pi, and Claude Code
   already do well.** Maintaining a curated set of agent prompts
   and a multi-stage planning harness inside Ligus would mean
   keeping up with the state-of-the-art in agent design — which
   is the whole industry. Better to host the agents you bring.
2. **The product story is "bring your own."** If your team uses
   aider for code edits and Claude for reviews, point Ligus at
   those. Ligus doesn't need its own Coder.

## Bring-your-own agent

Any LLM (API or CLI) becomes a Ligus agent via a runtime + a
WorkspaceAgent pointing at it.

=== "API agent (Anthropic)"

    1. Configure a runtime `anthropic` in Settings → Models.
    2. Add a WorkspaceAgent `@claude` pointing at it.
    3. `@claude` participates in chats.

=== "Subprocess agent (aider)"

    1. `pip install aider-chat`.
    2. Configure a runtime `aider` with `endpoint =
       "/usr/local/bin/aider"` in Settings → Models.
    3. Add a WorkspaceAgent `@coder` pointing at it.
    4. `@coder` writes code through aider when mentioned.

=== "Peer-owned agent"

    Bob has Claude Code on his machine; he doesn't want to give
    you his API key. He configures `@bob-claude` with
    `ownerActorID = bob`. When you `@bob-claude` in chat:

    1. Ligus routes the message to Bob's device over the P2P link.
    2. Bob's instance runs Claude Code locally with his subscription.
    3. The reply flows back through the envelope log.

    No credentials leave Bob's machine.

See [BYO agents](../addons/byo-agents.md) for the full configuration
recipe.

## Tool capabilities

Different runtimes expose different capabilities:

| Runtime kind        | What it can do                                          |
|---------------------|---------------------------------------------------------|
| API providers       | Enabled MCP tools (whatever MCP servers you've turned on for the chat). |
| Claude Code         | Claude Code's own tools, governed by its own permission mode. |
| Google Antigravity (`agy`) / GitHub Copilot (`copilot`) | The CLI's own tools, with Ligus's Claude Code permission setting mapped to the CLI's flags (read-only by default). Uses your Google AI Pro / Ultra or Copilot subscription; see [Subscription CLIs](../addons/byo-agents.md#subscription-clis-google-antigravity-and-github-copilot). |
| aider / pi          | The agent's own internal tools. Ligus captures stdout as the reply. |
| Ollama / local OpenAI-compatible (`custom`) | Ligus's built-in file tools (on a local or private endpoint) plus enabled MCP tools when the runtime's **Supports tools** switch is on (for Ollama, also when the model's capability probe reports tools); text only otherwise. At most 16 tools a turn, see [MCP servers](../addons/mcp.md#local-models-at-most-16-tools-a-turn). |
| ligus-daemon         | Text only (no tool calls in either direction yet). |

MCP servers are inert until you enable them, so an API runtime starts with no
tools until you turn some on in the Tools pane.

## Agent dispatch model

When you send a message with no `@mention`, the message goes to the
chat's primary runtime. When you `@agent`:

1. Ligus resolves the mention to a workspace agent.
2. If the agent runs on this device, Ligus calls the runtime locally.
3. If the agent runs on a peer's device, Ligus routes the message
   through the P2P link (relay-forwarded when no direct link exists).
4. The reply comes back as a normal assistant message in the log.

This is the same path whether the peer is on your LAN, across the
internet via the rendezvous relay, or via the relay-forwarded
fallback path.

Every agent has a **host** — the device its turns run on — shown as a
badge on the agent: **This device** (a local persona) or a named
**worker** (a headless box enrolled under People → Remote agents, with
online status). You pick the host when creating an agent, and can
reassign it later. The host determines which machine — and therefore
which code directory — the agent's turns touch, so routing code work to a
specific tree is a matter of `@mention`-ing the agent on the right host.
See [Headless agents](../../operate/concepts/headless-agents.md).

### One turn engine on every host

Whichever host runs a turn (the desktop, a `ligus worker` or `ligus serve`
box, or the [`lt` terminal app](https://github.com/langstroth-ai/ligus-term)), the turn runs through the same engine in
the Ligus runtime. The host decides the turn's policy first: the author
gate (the role the trigger's author held when they posted), the shell rule
(the shell is the workspace Owner's unless an Owner granted it to the
agent), the chat's sandbox envelope and, for a chat with a repository, that
this host has a checkout of it. The engine then runs the turn with that
policy, the same way everywhere:

- tools: Ligus's built-in file tools for a runtime that runs the tool loop
  (on a local or private endpoint), the enabled MCP tools, and the Ligus
  tools (propose, hand off, ask, read);
- edits: a CLI agent that writes runs in a per-turn git worktree cut up
  front; a model with the built-in tools gets one on its first write. The
  checkout itself is never edited, and the turn's diff comes back as a
  proposal;
- every process the turn starts (the CLI, `run_command`, a stdio MCP
  server, including while its tools are listed) runs in the host's agent
  sandbox under the turn's policy.

What the host keeps is how it shows the turn (the desktop's window, a
phone's live events, `lt`'s tool activity under the reply) and its own
settings and catalogs.

## Who can use an agent, and who pays {#who-can-use-an-agent}

!!! tip "The settings, step by step"
    [Who can use your agents](../features/agent-access.md) walks through the
    controls, the replies others see and the `@ligus` privacy switch. This
    section is the full rule set.

An agent's turn runs on its owner's device, or on the worker it was sent to,
**on their credentials**: their API key, their Claude Code, Codex or Copilot
subscription, their local model, or the worker's runtime. So the agent's
owner decides who may start those turns, and the device that would run a
turn enforces it.

### Who can use it

| Agent | Default | Who can change it |
|---|---|---|
| Personal (it has an owner) | **Only me**: only its owner | its owner, and only its owner, whatever anyone else's role |
| Shared (no owner) | **Everyone** who can post | an Admin; restricted, it is usable by workspace Admins and Owners |

The owner sets **Who can use this agent** to **Everyone in the workspace**
in the agent's card (right-rail **Tools** → **Workspace** → **Agent roster**; see [Who can use your agents](../features/agent-access.md)). The setting is part of the agent's signed
record (`access`: `"owner"` or `"everyone"`), so every device sees the same
value, and every device's fold refuses a change to it, or to the agent's
budgets, by anyone but the owner (an Admin on a shared agent): an Admin's
rename that keeps the settings stands, one that opens the agent up is
dropped. A value a device doesn't know is read as "Only me", so an older
device never opens an agent up. In a workspace on rules v2, **opening** an
agent to everyone (or creating one already open) needs its owner's passkey
confirmation, like rebinding its host or runtime; narrowing it, and changing
its budgets or notice, don't.

A turn runs only if **every person behind it** may use the agent: the
message's author, every person an agent-to-agent chain reaches (an
`@mention` cascade or a hand-off: the person who started it), and a workflow
stage's run initiator, and whoever signed a hand-off card that wakes the
agent on that message. Asking your own agent to ask someone else's doesn't
get around it. A chain no person can be traced back to runs only on an agent
open to everyone. The router never offers an un-addressed message to an
agent its author can't use.

The `@mention` autocomplete greys out an agent you can't use and says why;
the phone gets the same as `available` and `unavailableReason` on
`GET /v1/agents`.

### What a refused turn looks like

The device that would run the agent answers the message with one short
reply in the agent's name that no model wrote, for example *"scout is only
available to its owner."* Nothing is claimed, no model is called, nothing is
counted. Because the reply answers the message, no other device picks the
turn up afterwards. A member gets at most one such note per agent every five
minutes; further refusals in that window are silent (the turn is still
refused, and the device doesn't judge it again). A note says only what the
agent's signed settings already say: never its prompt, its runtime, or how
far into its plan window its owner is.

### The offline fallback

When the device that runs an agent stays silent (your teammate's laptop is
closed), the sender's device normally answers the turn itself after a
short wait, so a chat is never left hanging. For an agent the sender may not
use, that fallback **refuses** in the same way instead: someone else's
personal agent never runs on the sender's device on its owner's behalf. Ask
again when the owner's device is back, or ask its owner to open it up.

### Daily budgets

An owner can also cap use per day (UTC days, resetting at 00:00 UTC):

| Budget | Default | Notes |
|---|---|---|
| Turns per member per day | 50 on a personal agent; no limit on a shared one | `0` means no limit. The owner's own turns are never limited. A turn is counted against the person who asked (the author of the message the chain starts from), and every other person behind it must be under the cap too: asking through someone else's agent doesn't reset yours. |
| Estimated spend per day (USD) | none | Counts everyone's turns, the owner's included; once reached, other members' turns are refused and the owner's still run. |

Spend is estimated from the token usage the runtime reports, priced by the
same table as the per-turn [spend budget](../addons/ligus-mcp.md#spend)
(`LIGUS_MODEL_PRICES` adds or overrides prices). A runtime that reports no
usage (most subscription CLIs, local models) can't be counted in dollars,
only in turns. The cap is checked before a turn starts, so the turn that
crosses it finishes (and turns already running when it is crossed). A turn
is counted when it is admitted, in the same step as the check, so two turns
can't both pass a cap of one; a turn that then fails still counts.

Usage is counted in the database of **the device or worker that runs the
agent**, never synced: an agent its owner runs on two devices is counted on
each separately. A refused turn never reaches the model and counts nothing.

### Seeing who uses it

- **Usage.** The owner sees, per member, turns today and this month and the
  estimated cost where known, in the agent's settings (desktop), through
  `GET /v1/agents/{id}/usage` on a `ligus serve` box (the box's own counts,
  owner only), and in `lt`'s agents view and plain mode. Each shows what
  that device counted.
- **Notify me when someone else uses this agent.** With it on, the owner's
  desktop shows a notification when another member's turn runs on the
  agent, whether it ran on that desktop or elsewhere (seen when the reply
  syncs). A phone-only owner reads the usage view instead.

### Capacity: your own requests first

A subscription CLI's plan window (Claude Code's five-hour limit, a Copilot
quota) is shared by everything that runs on it. Your own direct requests to
your agent always run. Another member's direct mention of your agent counts
as **background work** on your runtime: it is the first to pause as the
window fills (from about 70%), with a short note in the chat, leaving the
headroom for you. The desktop app applies this (it is where plan windows are
tracked); a worker has no plan-window throttle.

### `@ligus`, the chat's primary runtime {#primary}

`@ligus` isn't a workspace agent: it runs on the device of the person who
created the chat, on **their** credentials. So it follows the same rule as a
personal agent. In a chat with other people it answers only that device's
person; another member's `@ligus` gets a short note instead (*"Ligus in this
chat runs on another member's device and account, and only answers them.
..."*). Its person can open it in **Settings → Profile → Privacy → Let
members use @ligus on this device** (a device setting, not synced). Alone in
a workspace, and in your own local workspace, it always answers. The same
applies to a workflow stage run by `@ligus`, and to `ligus agent`, which
answers only its operator unless `LIGUS_AGENT_OPEN_TO_MEMBERS=1`.

## Reviewing agent changes

Ligus has no inline per-tool consent prompt of its own. Trust is enforced at
three real seams instead:

- **MCP tools are inert until enabled.** An API runtime can't call a tool you
  haven't turned on for the chat.
- **Claude Code brings its own permission mode.** It gates its own file and
  command actions; Ligus doesn't second-guess it.
- **Changes land as proposals you review.** Edits an agent makes surface in
  the **Diff** tab and the **Review** pane, where peers approve (and reach
  quorum, if configured) before they're implemented.

Those decisions are events too; they sync to peers and survive relaunches.

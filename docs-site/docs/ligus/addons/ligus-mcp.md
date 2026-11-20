# Ligus tools for agents (MCP)

Every agent turn that can use tools gets **Ligus's own tools**: typed calls
for filing proposals, handing off, leaving marks, reading the workspace and
asking the people in the chat. They replace the `[[propose: …]]`-style reply
directives. Directives still work for runtimes without tool use.

The tools come from an MCP server inside the Ligus process. Agents reach it
in one of three ways:

- **CLI agents** (Claude Code, Codex, Copilot, Antigravity) get it as a stdio
  MCP server, `ligus mcp-bridge`. Over [ACP](acp.md) it's in `session/new`;
  on the headless adapters it goes through Claude Code's `--mcp-config` and
  Copilot's `--additional-mcp-config`. Codex and Antigravity on their
  headless adapters use directives instead.
- **API and local models** that run Ligus's tool loop (Anthropic, and
  OpenAI-compatible endpoints with "Supports tools") get the same tools
  folded into four (`ligus_propose`, `ligus_flow`, `ligus_read`,
  `ligus_ask`), called in-process. The code-host reads fold into
  `ligus_read` as `what: forge.prs | forge.pr | forge.issues | forge.issue |
  forge.ci | forge.file`.
- **aider, pi, Hermes**, and any runtime without tool use, keep the
  directives.

A prompt teaches the directives only when the tools aren't offered. If a
reply has both, the tool calls win and the duplicate directives are dropped.

## The tools {#tools}

Every tool files or reads something. None of them applies, merges, votes or
changes workspace settings.

| Tool | Input | What it does |
|---|---|---|
| `ligus_propose_diff` | `title`, `body`, `taskClass?` | Files the changes in your worktree for review when your turn ends, under this title |
| `ligus_propose_decision` | `title`, `body`, `requiredApprovals?` | A decision for the team to approve. `requiredApprovals` can only raise the default |
| `ligus_propose_command` | `title`, `body`, `argv[]`, `cwd?` | A command. On approval, Implement runs **exactly this argv** in the chat's worktree, sandboxed, with no network and no model in between |
| `ligus_propose_workflow` | `definition` | A workflow, validated now and added only when a person approves it |
| `ligus_open_alternatives` | `question`, `body?`, `taskClass?`, `agentQuorum?`, `options[2..6]` | A [proposal set](../features/proposal-sets.md). `agentQuorum` can only raise the default |
| `ligus_assess` | `setId`, `alternative`, `score 0-100`, `rationale` | A scout's assessment |
| `ligus_handoff` | `to`, `keyFiles?`, `decisions?`, `ruledOut?`, `notes?` | A hand-off card with a context parcel. **The receiving agent's turn starts right away**, on whatever host runs it. A hand-off to a person notifies them |
| `ligus_mark` / `ligus_unmark` | `kind: claim\|lock\|review`, `path`, `lines?`, `note?` / `id?\|path?\|kind?` | Workspace marks (an agent clears only its own) |
| `ligus_react` | `messageId?`, `emoji` | A reaction |
| `ligus_list_chats` | `channel?`, `limit ≤ 50` | The workspace's chats |
| `ligus_read_chat` | `chatId?`, `sinceMessageId?`, `limit ≤ 100` | Messages from **any chat in the workspace** whose egress is no stricter than this one's |
| `ligus_read_proposals` | `chatId?`, `status?`, `setId?` | Proposals |
| `ligus_read_board` | | Marks, hand-offs and open questions |
| `ligus_ask_human` | `question`, `options? ≤ 6`, `timeoutSecs ≤ 3600` | Asks the chat's people. **The turn pauses** until someone answers or the timeout passes (then `{"error":"no_answer"}`) |
| `ligus_report` | `status: done\|blocked\|needsReview`, `summary ≤ 400`, `filesTouched?`, `confidence?` | A short self-report, shown on the turn. Never used for scoring |
| `ligus_offer_file` | `path`, `title`, `body?` | Offers a file from your worktree as a proposal, only when the workspace lets agents offer files. Nothing is shared until a person approves it; blocked types and files holding a host credential are refused. See [Sharing files](../features/attachments.md#files-from-agents). The in-process tool loops (API and local runtimes) don't have it |
| `ligus_memory_search` | `query`, `tags? ≤ 8`, `limit ≤ 20` | Keyword search over the [workspace memory](../features/memory.md): every word must match |
| `ligus_memory_read` | `entryId` | One note, its current version (`rev`) and any unsettled conflicts |
| `ligus_memory_propose` | `op: create\|edit\|remove`, `entryId?`, `base?`, `title?`, `body?`, `tags?`, `rationale?` | Proposes a memory change for a person to accept or reject. An edit or removal names the `rev` it was made from. Memory doesn't change until a person accepts |
| `ligus_task_update` | `status: working\|blocked\|waiting\|done\|abandoned`, `summary ≤ 400`, `progress? 0-100`, `openQuestions? ≤ 8`, `partialResults? ≤ 8`, `taskId?`, `ttlSecs? ≤ 86400` | Your live progress on this chat, seen by its people and agents as you go; each call replaces the last |
| `ligus_task_state` | `chatId?` | The agents' live task state on a chat (default: this one), under the same egress rule as `ligus_read_chat` |
| `ligus_forge_list_prs` | `repo?`, `state?: open\|closed\|all`, `limit ≤ 50` | Pull or merge requests on the chat's [code host](../features/code-hosts.md) |
| `ligus_forge_read_pr` | `repo?`, `number` | One pull or merge request with its review comments and CI |
| `ligus_forge_list_issues` / `ligus_forge_read_issue` | `repo?`, `state?`, `limit?` / `repo?`, `number` | Issues |
| `ligus_forge_ci_status` | `repo?`, `ref` | Checks and commit statuses of a branch, tag or commit |
| `ligus_forge_read_file` | `repo?`, `path`, `ref?` | A text file at a ref |

A hand-off's receivers are claimed one by one: when a reply hands off to
two agents that run on different devices, each device runs its own
receiver exactly once.

The JSON Schemas are in
`crates/ligus-runtime/src/ligus_mcp/schema/` and are frozen per release: a
test fails when one changes.

Results from the read tools carry a header that says they are data, not
instructions. Reading other chats widens what a prompt injection in one chat
can reach, so treat a chat's contents accordingly. A chat that is locked
down (or limited to an MCP allow-list) can't be read from a chat with wider
egress: `ligus_read_chat` and `ligus_read_proposals` refuse it, so its
content stays where its egress is cut.

The `ligus_forge_*` tools are read-only: there is no tool to open a pull
request, fork, comment or push. They run in the host, so the code-host token
never enters the agent's sandbox. `repo` is the chat's bound repository
(default) or, for a fork, its upstream; nothing else. A chat with closed
egress gets none of them, an allow-listed chat only when its list names
`ligus.forge`, and each turn may make at most 16 of them
(`LIGUS_MCP_MAX_FORGE_CALLS`).

### Memory and task state {#memory}

There is no tool that writes memory or decides a proposal: an agent only
proposes, signed with its own key (or, before its host has certified one, by
the host marked as an agent's effect, never as the host's person), and every
device's fold refuses a direct memory write from anything but a person.
`ligus_memory_search`, `ligus_memory_read` and `ligus_task_state` return a
`content` text in which each note or card is wrapped in
`<<<entry NONCE …>>> … <<<end NONCE>>>` (or `<<<task …>>>`) with a random
nonce per call, after a header that says it is data, not instructions;
content can't contain the markers, the nonce or `[[` directive openers,
and invisible characters (including Unicode tag characters) are dropped.
A turn sees only the notes its chat's egress allows: a note written from a
chat with restricted or closed egress (a lockdown, dearth mode) is left out
of searches and refused by id for a turn whose egress is more open (the
same rule as reading another chat). Each note read counts against the read
budget. Tool-loop runtimes reach the
same through `ligus_read` (`what: memory | memoryEntry | taskState`),
`ligus_propose` (`kind: memory`, with `op`) and `ligus_flow`
(`op: taskUpdate`). See [Workspace memory and task state](../features/memory.md).

## Who signs what {#signing}

An agent's effects are signed with **the agent's own key**. The host it runs
on certifies that key in the workspace (`AgentKeyCertified`), and the key is
valid only while the agent is bound to that host. Every device judges these
events at the Agent tier:

- proposals filed only as the agent itself, open and without votes;
- marks, hand-offs, reactions, assessments and its own questions;
- never a vote, an apply, an admin or governance change, a workflow
  definition, a lockdown release, or an answer to a question.

The server checks the same limits before it signs anything. A host that
can't certify a key (it isn't certified in the workspace yet) signs as
itself, and every device still judges the event at the Agent tier.

## The per-turn credential {#credential}

Each turn's tools are bound to that turn. The host mints a credential, an
HMAC under a key that exists only in the host's memory
(`ligus-mcp-turn-cred-v1`), that names the chat, the agent and the turn and
expires with it. The bridge presents it in MCP `initialize`. Every call is
checked, and the turn's id is revoked when the turn ends, is stopped or
times out. The credential never leaves the device and isn't synced.

The socket the bridge connects to lives in a fresh `0700` directory. It's
the one extra path the [agent sandbox](../../operate/security/index.md#guards) allows,
under Seatbelt and bubblewrap alike. The Ligus server is local, so it's
available even when a chat's egress is closed.

## Budgets {#budgets}

Per turn, and set per device:

| Budget | Default | Variable |
|---|---|---|
| Tool calls (every call, unknown tools included) | 64 | `LIGUS_MCP_MAX_TOOL_CALLS` |
| Proposals (any kind) | 8 | `LIGUS_MCP_MAX_PROPOSALS` |
| Hand-offs | 2 | `LIGUS_MCP_MAX_HANDOFFS` |
| Questions (ACP permission requests included) | 3 | `LIGUS_MCP_MAX_ASKS` |
| Memory proposals | 4 | `LIGUS_MCP_MAX_MEMORY_PROPOSALS` |
| Messages, proposals, memory notes and task-state cards read | 500 | `LIGUS_MCP_MAX_READ_MESSAGES` |
| Spend (USD) | unset | `LIGUS_MCP_MAX_SPEND_USD`; see [Spend budget](#spend) |

One human message can fan out: a turn hands off, the receiver hands off
again, up to the chat's cascade depth. So every turn that descends from the
same human message (through hand-offs and replies to agent replies) also
draws on a **cascade budget**, shared by all of them on this device:

| Cascade budget | Default | Variable |
|---|---|---|
| Tool calls | 192 | `LIGUS_MCP_MAX_CASCADE_TOOL_CALLS` |
| Proposals | 12 | `LIGUS_MCP_MAX_CASCADE_PROPOSALS` |
| Hand-offs | 6 | `LIGUS_MCP_MAX_CASCADE_HANDOFFS` |
| Questions (ACP permission requests included) | 6 | `LIGUS_MCP_MAX_CASCADE_ASKS` |

An ACP permission request that would go to a person counts as a question:
past either question budget it is denied without asking anyone.

When a budget runs out, the call returns
`{"error":"budget_exhausted","budget":"…"}` (`maxProposals`,
`cascadeProposals`, …) along with the note that tells
the model to finish its answer.

### Spend budget {#spend}

`LIGUS_MCP_MAX_SPEND_USD` caps what one turn may cost. When the turn
reaches it, Ligus **ends the turn** (an ACP agent gets `session/cancel` and
is stopped; an API request's stream is closed) with an error that says
so, and any further Ligus tool call is refused. The budget holds on turns
without the Ligus tools too.

What is counted:

- **ACP agents:** the cost in their `usage_update` reports. A report must
  name its currency as USD and never go down; a negative, decreasing,
  non-USD or currency-less report ends a budgeted turn (without a budget it
  is ignored). An agent that reports nothing, or reports less than it
  spent, isn't held by the budget: this counts what the agent says.
- **Anthropic, OpenAI, OpenRouter and Azure runtimes:** the token usage
  the API reports, priced by Ligus. Anthropic models use the list prices
  (cache reads at 0.1x, cache writes at 1.25x input); a model Ligus doesn't
  know, including every OpenAI model, is priced at the highest rate
  ($15/$75 per million tokens), so it can only end a turn early. Set
  `LIGUS_MODEL_PRICES` (`prefix=in/out,…`, USD per million tokens, for
  example `gpt-4o=2.5/10`) on the device to price yours.
- Legacy CLI adapters (`claude -p`, `codex exec`, …), aider, pi, Hermes,
  Ollama and custom endpoints report no cost and aren't counted.

The budget is per turn. A hand-off starts a new turn with its own budget.

`LIGUS_MCP_MAX_SPEND_USD` is the device's cap for **one turn**. An agent's
owner can also set a **daily** estimated-spend cap and a per-member daily
turn count on the agent itself, counted by the device that runs it from the
same reported usage and prices; the per-turn budget still ends a single
expensive turn, and the daily cap refuses other members' new turns once the
day's spend reaches it. See
[Daily budgets](../concepts/agents.md#daily-budgets).

## Audit {#audit}

- **Every call** is a device-local, content-free fact: time, turn, chat,
  agent, tool, outcome (`ok`, `denied`, `budget`, `error`), latency, and for a
  read, which chat it read. `ligus facts export` includes these lines. Past
  the tool-call budget nothing runs and only the first refusal is recorded,
  so one turn can't flood the audit log.
- **Every turn** that used the tools adds one synced `TurnToolSummary` to the
  chat: calls per tool, denials, and whether a budget ran out. Teammates see
  what an agent did without seeing content.

## Questions to the chat {#asks}

`ligus_ask_human` (and an ACP permission request that needs a person) posts
the question in the chat. It shows in the [Review pane](../features/right-rail.md)
and is pushed to paired phones as `ask.pending`. Anyone in the chat can
answer, and the first answer wins. Answering a `ligus_ask_human` question
doesn't authorize anything by itself, so it needs no passkey. A permission
request is different: it is granted only by the host's own person or the
agent's owner (see [ACP permissions](acp.md#permissions)). An agent can't answer a question. If the host
restarts while a turn is waiting, the turn is cancelled. A question can't be
resumed in 0.1.

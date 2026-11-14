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
  `ligus_ask`), called in-process.
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
| `ligus_open_alternatives` | `question`, `body?`, `taskClass?`, `agentQuorum?`, `options[2..6]` | A [proposal set](../features/proposal-sets.md) |
| `ligus_assess` | `setId`, `alternative`, `score 0-100`, `rationale` | A scout's assessment |
| `ligus_handoff` | `to`, `keyFiles?`, `decisions?`, `ruledOut?`, `notes?` | A hand-off card with a context parcel. **The receiving agent's turn starts right away**, on whatever host runs it. A hand-off to a person notifies them |
| `ligus_mark` / `ligus_unmark` | `kind: claim\|lock\|review`, `path`, `lines?`, `note?` / `id?\|path?\|kind?` | Workspace marks (an agent clears only its own) |
| `ligus_react` | `messageId?`, `emoji` | A reaction |
| `ligus_list_chats` | `channel?`, `limit ≤ 50` | The workspace's chats |
| `ligus_read_chat` | `chatId?`, `sinceMessageId?`, `limit ≤ 100` | Messages from **any chat in the workspace** |
| `ligus_read_proposals` | `chatId?`, `status?`, `setId?` | Proposals |
| `ligus_read_board` | | Marks, hand-offs and open questions |
| `ligus_ask_human` | `question`, `options? ≤ 6`, `timeoutSecs ≤ 3600` | Asks the chat's people. **The turn pauses** until someone answers or the timeout passes (then `{"error":"no_answer"}`) |
| `ligus_report` | `status: done\|blocked\|needsReview`, `summary ≤ 400`, `filesTouched?`, `confidence?` | A short self-report, shown on the turn. Never used for scoring |

The JSON Schemas are in
`crates/hive-runtime/src/ligus_mcp/schema/` and are frozen per release: a
test fails when one changes.

Results from the read tools carry a header that says they are data, not
instructions. Reading other chats widens what a prompt injection in one chat
can reach, so treat a chat's contents accordingly.

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
the one extra path the [agent sandbox](../security/index.md#guards) allows,
under Seatbelt and bubblewrap alike. The Ligus server is local, so it's
available even when a chat's egress is closed.

## Budgets {#budgets}

Per turn, and set per device:

| Budget | Default | Variable |
|---|---|---|
| Tool calls (all Ligus tools) | 64 | `LIGUS_MCP_MAX_TOOL_CALLS` |
| Proposals (any kind) | 8 | `LIGUS_MCP_MAX_PROPOSALS` |
| Hand-offs | 2 | `LIGUS_MCP_MAX_HANDOFFS` |
| Questions | 3 | `LIGUS_MCP_MAX_ASKS` |
| Messages read | 500 | `LIGUS_MCP_MAX_READ_MESSAGES` |
| Spend (USD) | unset | `LIGUS_MCP_MAX_SPEND_USD`, counted where the runtime reports cost (ACP `usage_update`) |

When a budget runs out, the call returns
`{"error":"budget_exhausted","budget":"…"}` along with the note that tells
the model to finish its answer.

## Audit {#audit}

- **Every call** is a device-local, content-free fact: time, turn, chat,
  agent, tool, outcome (`ok`, `denied`, `budget`, `error`), latency, and for a
  read, which chat it read. `ligus facts export` includes these lines.
- **Every turn** that used the tools adds one synced `TurnToolSummary` to the
  chat: calls per tool, denials, and whether a budget ran out. Teammates see
  what an agent did without seeing content.

## Questions to the chat {#asks}

`ligus_ask_human` (and an ACP permission request that needs a person) posts
the question in the chat. It shows in the [Review pane](../features/right-rail.md)
and is pushed to paired phones as `ask.pending`. Anyone in the chat can
answer, and the first answer wins. Answering doesn't authorize anything by
itself, so it needs no passkey. An agent can't answer a question. If the host
restarts while a turn is waiting, the turn is cancelled. A question can't be
resumed in 2.0.

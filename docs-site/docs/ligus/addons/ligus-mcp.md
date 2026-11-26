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
| `ligus_read_proposals` | `chatId?`, `status?`, `setId?`, `archived?` | Proposals in the inbox; `archived: true` adds archived ones and ones hidden on this device |
| `ligus_read_board` | | Marks, hand-offs and open questions |
| `ligus_ask_person` | `question ≤ 500`, `options` (2 to 5, each ≤ 60) or `yesNo`, `allowText?`, `to?: requester\|<member id>`, `timeoutSecs 10-3600` | A [decision card](#asks) for one person: by default the one who started this turn. **The turn pauses** until they answer, say Not now, or the timeout passes |
| `ligus_ask_room` | as `ligus_ask_person`, with `minRole?: contributor\|admin\|owner` and `quorum? 1-5` instead of `to` | A decision card for anyone at or above `minRole`, or a quorum of them who must pick the same option |
| `ligus_ask_human` | `question`, `options? ≤ 6`, `timeoutSecs ≤ 3600` | The older question tool, unchanged: anyone at Contributor or above answers, the first answer wins (else `{"error":"no_answer"}`). Prefer the two above |
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

An agent that needs one word from a person asks with a **decision card**:
`ligus_ask_person` (one person, by default whoever asked for the turn) or
`ligus_ask_room` (anyone at or above a role, or a quorum). The card has the
question, 2 to 5 options or Yes/No, an optional free-text reply, and **Not
now**. It shows in the chat where the agent asked, in the Review pane, as an
Inbox row and in `lt`, and is pushed to the phones of the people it asks.

- **Who may answer.** Only the people the card names. Others see it
  read-only ("Waiting on Ana", "Waiting on Admins · 1 of 2"). A room's role
  is judged at the answer's own position in the workspace's history, so a
  demoted member can't answer by back-dating. Every answer is a signed event
  bound to a digest of the exact card it answers: change the question, the
  options or who it asks, and the answer counts for nothing.
- **Quorum.** With `quorum: 2`, two different people must pick the same
  option. Each person's latest answer is their vote; once an option has
  enough, the card is settled and stays settled.
- **Not now** closes a card put to one person (`declined`). On a room card
  it is only that person's abstention: the card stays open for the others.
- **What an answer does.** Nothing by itself: the agent reads it and goes on
  within what it could already do. The card says so in Ligus's words ("Your
  answer grants no new permission"). A change that needs approval is a
  proposal, not a question.
- **Plain text.** The question and options are the agent's words, shown as
  plain text: no Markdown, no clickable links. Bidi, zero-width and control
  characters are removed, options are one line, and an option may not be
  one of Ligus's own control labels (Allow once, Approve, Approve & apply,
  Deny, Not now, Reject): the tool refuses it with `option_reserved`.
- **Limits.** A card shares the asks budget (3 per turn, 6 per cascade; past
  it `{"error":"ask_budget"}`, nothing posted). An agent may have 2 open
  cards in a chat and a chat 5 (`too_many_open`). A quorum larger than the
  people who could answer now is refused (`quorum_unreachable`).

The tool returns one of:

```json
{"status": "answered", "optionIndex": 0, "option": "Postgres", "text": "keep the dev db",
 "answeredBy": [{"memberId": "…", "name": "Ana", "at": "2026-11-24T10:02:11Z"}]}
{"status": "no_answer", "tally": [1, 0]}
{"status": "declined"}
{"status": "cancelled"}
```

Every status but `answered` is an error result: the agent treats it as no
and doesn't ask again this turn. `tally` comes only with a quorum.

**Permission requests** from an ACP agent are **grant cards** the host
posts (see [ACP permissions](acp.md#permissions)): "Allow once lets Builder
run this once on Ana's mbp", the call's own content (the command, the files
and their new content), and the agent's own title labelled as its words.
Only the host's person or the agent's owner can grant one. The host asks for
a passkey when the chat is in lockdown, dearth mode is on, or the call
reaches outside the turn's sandbox. A call too large to show on a card
(over 8 KiB), or one whose agent sent no details of it, gets no Allow.
Every input of the call is shown (a working directory, environment, a
sandbox switch), and hidden characters in it are shown as visible escapes
such as ⟨U+200B⟩, so what you allow is what you saw. Only the host that runs the turn can post a
grant card; one anybody else signs is drawn as their plain question.

`ligus_ask_human` still works as before: anyone at Contributor or above
answers and the first answer wins. Its card is plain text too. Only the
asker (the agent or its host) closes a question, and an agent can't answer
one. When a host restarts, its turns are cancelled and it closes the
questions they were waiting on. A question can't be resumed.

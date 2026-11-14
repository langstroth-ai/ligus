# Flow: routing, hand-offs and backpressure

Flow is how work moves through a workspace: who picks up a task nobody
assigned, how an agent hands work to another, where work waits, and what
agents leave in the workspace for each other. Ligus is short for
*ligustica*, the Italian honeybee, and several of these mechanisms follow how
a colony allocates labour without a manager. The bee story is only the
inspiration; each feature below is ordinary engineering you can turn on,
off, or watch in shadow mode.

Everything lives in the **Flow** pane of the [right rail](right-rail.md)
(Board, Queue, Marks, Routing, Roster, Zones, Home) and in
**Settings → Flow**. Flow settings are per device. Everything Flow records
rides the workspace's end-to-end encrypted event log like messages do; an
older Ligus ignores it.

## Hand-off cards and context parcels

When an agent's reply hands work to another agent (it `@mentions` one), or a
workflow stage feeds the next, Ligus records a **hand-off card** under the
handing-off message:

- **what**: a one-line task summary and its task class;
- **where**: the chat's branch and the files involved;
- **how good**: measured facts: whether the last test command passed, the
  state of the proposal it filed, how many turns it took.

An agent can attach a **context parcel**, a short summary so the receiver
starts primed instead of rereading the chat, by ending its reply with:

```text
[[handoff: {"keyFiles": ["src/parser.rs"], "decisions": ["kept the old API"],
            "ruledOut": ["a regex rewrite"], "notes": "edge cases in tests/p.rs"}]]
```

Any runtime can do this (Claude Code, Codex, Aider, Pi, an HTTP model, an API
turn with MCP tools): it is plain text in the reply, stripped before the reply
is shown, and it uses no tool slot. The receiver's next turn gets the card and
parcel in its latest message, and Ligus records how long the hand-off waited
to be picked up.

## Routing un-addressed work

When you post a message without naming anyone, it normally goes to the
primary. With routing on, Ligus picks among the chat's agents instead:

- Each agent has a **score per task class** (refactor, bugfix, docs, review,
  test, triage, …): a decayed average of outcomes Ligus measured itself:
  proposals approved or rejected, revise loops, workflow stage results, failed
  turns, and the exit status of test commands the tool runner saw. Old
  outcomes fade (half-life 14 days by default). **An agent's own confidence
  is never an input.**
- The pick is **sampled in proportion to the scores**, not the best score
  every time, so a weaker agent still gets some work and can recover.
- **Your choice always wins**: an `@mention` never reaches the router.
- **Private memory comes next**: the agent already carrying the chat's routed
  thread keeps it, and the agent with the latest approved change to a file
  the message names gets it.
- An agent sent to a host that is offline is not a candidate, and an agent on
  [probation](#careers) only gets low-risk work.
- Neither is an agent on a runtime the chat's
  [sandbox envelope](../concepts/security.md#the-sandbox-envelope-around-every-agent-turn)
  refuses while egress is restricted, or on a runtime whose
  [plan window](guards.md#plan-windows-staggered-throttles) is past the
  hand-off threshold. The primary always remains.

Routing ships in **shadow mode**: the primary still answers, and Ligus logs
what the router *would* have picked. **Flow → Routing** shows those
decisions, the scores with their trend, and outcomes per arm (shadow vs
routed): acceptance rate, turns per accepted proposal, and the share of work
that ran on a local model. When the numbers look right, set **Settings → Flow
→ Route un-addressed messages** to **Live**.

Every device that could answer a message makes the same decision: the first
decision recorded for a message is honoured everywhere, and otherwise the draw
is seeded by the message id.

### Exploration

A share of decisions deliberately goes to an agent other than the top scorer.
The share adapts between two bounds (5 % and 30 % by default): it rises when
the top scorer is weak, has few outcomes, or is slipping, and falls when it
is strong and steady. Flow → Routing shows how often the router explored and
how explored turns did against the top scorer's own.

### Cool-down after failures

A route is taken out of rotation at once after a severe failure (a rate
limit, an exhausted plan window, a provider outage, an unreachable local
server) and after any other failure that repeats within ten minutes. A
provider-wide failure pauses every agent on that runtime. The cool-down is 15
minutes by default (0 turns it off). Stopped routes are listed in Flow →
Routing with a **Clear** button; an `@mention` still reaches a stopped agent.

## Backpressure: the review queue

Ligus measures waiting time where work is handed on: proposals waiting at
the review gate, workflow stages waiting for their host, and agent hand-offs
waiting to be picked up. **Flow → Queue** shows the gate (waiting proposals
against the queue limit, oldest and median wait, how many were opened and
decided in the last hour) and the waits per hand-off point, and raises an
alert that names both sides: where to add capacity, and that agents feeding
the gate should slow down.

- **Stop upstream**: while the review queue is over its limit (5 by default),
  agent turns are told to finish or refine open proposals before opening new
  ones, and a turn that files into the full queue posts a note in the chat.
  You can switch this to **A/B** to apply it in half the chats and compare.
- **Ask more reviewers** (opt-in): when the queue backs up, members who could
  approve and haven't voted get a notification, at most hourly.
- **Take queued stages when idle** (opt-in): a workflow stage that has waited
  over a minute for a busy or offline host runs on this device instead, if
  this device is idle and has the agent's runtime. The stage's turn claim
  decides, so the host and this device never both run it.

## The task board

Work nobody was asked to do (a chat whose latest test run failed, a proposal
no one has looked at, a workflow that failed at a stage, a needs-review
marker) lands on **Flow → Board** instead of defaulting to the primary.

Each item has a **stimulus** that grows with its age, its severity and how
much of its kind is waiting. Each agent has a **threshold** per kind, set by
its career and persona plus a small fixed per-agent offset, so two otherwise
identical agents don't react at the same moment. The free agent whose
threshold the stimulus crosses by the most is suggested; **Hand to …**
claims the item and posts the task in its chat, addressed to that agent.
Busy agents (an unanswered mention, a cool-down) are skipped, and an author
never reviews its own proposal. With **auto pickup** on (opt-in), agents that
run on this device pick items up on their own, one at a time.

## Careers

A new agent starts on **probation**: reviews, docs, tests and other low-risk
work. It **graduates** after five judged proposals with at least 60 %
approved. When the review queue is long, the strongest proposer is pulled
back to **reviewer duty** until it drains. Careers are recomputed from the
log, so they reverse on their own; you can pin any agent to a stage, or take
it off the task board, in **Flow → Roster**. Agents that existed before this
release start graduated. This is also a safe on-ramp for a newly added local
model.

The Roster tab also flags a **monoculture**: a roster whose agents all run on
one runtime fails, and agrees, all at once. Mixing in a different model or
provider makes the workspace steadier.

## Workspace marks

Agents and people can leave marks on files instead of mentioning each other:

| Mark | Meaning | Lasts |
|------|---------|-------|
| Claim | "I'm working on this" | 4 h |
| Under construction | A lock on a file or line range | 1 h |
| Needs review | Someone should look at this (it also goes on the task board) | until cleared |

Agents see the active marks in their context and leave their own with
`[[mark: {"kind": "claim|lock|review", "path": "src/x.rs", "lines": [10, 40], "note": "…"}]]`
and clear them with `[[unmark: {"path": "src/x.rs"}]]`. People use **Flow →
Marks**. If a turn's change touches a file another agent has locked or
claimed, the proposal says so; a person decides. Applying a change clears
its author's claims and locks on those files.

## Path zones

In **Flow → Zones**, an admin can mark **core** paths (high review bar) and
**low-bar** paths (docs, tests, generated files) by folder, file or glob
(`**/*.md`). A change's review bar comes from where it lands: any core file
raises it (two approvals from an admin by default; a core zone never lowers
a bar), and a change whose every file is low-bar may need fewer approvals,
never fewer than one. The task board keeps agents on probation away from
core paths.

Zones, [proposal sets](proposal-sets.md) and the
[workspace guards](guards.md) feed one review bar, and the strictest wins: a
set's dial can raise an alternative's bar but never lowers what its zone set,
and a low-bar zone can't lower the bar of a change to a dearth-mode protected
path or in a locked-down chat.

## Reusing worktrees

An isolated turn's worktree is kept for the chat's next turn instead of being
rebuilt: tracked changes and stray files are cleared, while ignored files
(build output, dependency caches) stay, so builds start warm. Up to two are
kept per chat, and deleting the chat removes them. Turn it off in Settings →
Flow.

The [hygiene sweep](guards.md#hygiene) knows about these: it never touches a
worktree a turn is using or one parked for a live chat, and it clears the
parked worktrees of a chat that is gone or archived. Parking and reuse are not
counted as leftovers.

## Home beacon

**Flow → Home** shows the workspace's home: the relay it syncs through, the
branch chats start from, and the host that coordinates it, with whether that
host is online. If this device's relay or base branch differs, it says so. An
admin publishes this device's setup as home; the beacon never contains a
token or passphrase.

## On the phone

The phone app doesn't show Flow yet: routing, hand-offs, marks and the board
work the same with or without it, since they run on the desktop and on hosts.
Reviewer notifications are shown by desktops for now.

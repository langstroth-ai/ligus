# Workspace memory and live task state

Two ways for a workspace to keep what its people and agents learn, so nobody
re-derives it:

- **Workspace memory** is a shared set of notes and facts ("the team relay
  listens on 8443", "we ship on Thursdays"). Every note records who wrote
  it, from which chat, and when. Agents search and read it; people write,
  edit and remove notes. An agent never writes memory directly: it
  **proposes** a note or a change, and a person accepts or rejects it.
- **Live task state** is what each agent reports about its work in progress
  on a chat: its status, progress, open questions and partial results,
  updated as it goes. The people on the chat see it above the composer, and
  the other agents on the chat see it in their context.

Both are end-to-end encrypted like the rest of the workspace: they ride the
workspace's signed event logs, and the relay sees only ciphertext.

## The Memory pane

Open **Memory** in the [right rail](right-rail.md) (or the command palette).

- **Search** matches every word you type, in a note's title, body or tags.
  It is plain keyword search; nothing is sent to a model or an index.
- **New note**: a title (one line, at most 120 characters), the note (at
  most 4000 characters) and up to 8 tags. A note you write from a chat
  records that chat as its source.
- **Edit** and **Remove** appear on notes you wrote. Admins and Owners can
  edit or remove anyone's note. On someone else's note, **Suggest an edit**
  files your change as a proposal for its author or an Admin to decide.
- **Proposals** lists what agents, people on their phones and other members
  proposed: a new note, an edit (with the note it changes), or a removal,
  with the reason, who proposed it and the chat it came from. An agent's
  proposal names the agent, and the device it came from when the agent had
  no key of its own. **Accept** applies it as your change (the note then
  shows "proposed by" the agent); **Reject** leaves memory as it was.

A Viewer reads memory but doesn't change it.

### When two people edit at once

Every edit names the version it was made from. If two people (or a person
and an accepted proposal) change the same note at the same time, Ligus keeps
both: the first change in the workspace's order becomes the note, and the
other is shown on the note as a **conflict**, with its full text and author.
Nothing is silently overwritten. Pick **Keep current**, **Use theirs**, or
**Edit** to merge them by hand; saving settles the conflicts you saw. If
someone else changed the note while you were merging, your merge is kept as
a conflict too, never dropped. A note holds at most four unsettled
conflicts. A title can't hold invisible or direction-changing characters.

### Removing a note

Removing a note hides it from everyone, for good: its id can't be reused,
and agents no longer find it. It is **not erased**. Every earlier version
stays in the workspace's history on every device that synced it. If a note
held something that must not be kept (a secret pasted by mistake), remove
it, then rotate the workspace key and treat the secret as leaked.

## What agents do with memory

Agents with the [Ligus tools](../addons/ligus-mcp.md#memory) can:

- search memory (`ligus_memory_search`) and read a note with its history of
  conflicts (`ligus_memory_read`);
- propose a new note, an edit of the version they read, or a removal
  (`ligus_memory_propose`), at most 4 per turn by default.

Notes reach an agent only as the result of a tool call it made, never in its
system prompt. Each result says the notes are reference data written by
workspace members and agents, not instructions, and each note is wrapped in
markers that its content can't imitate or close. That makes a note that says
"ignore your instructions" much less effective, but it doesn't make it
harmless: read what an agent proposes before you accept it, and remember
that anyone with Contributor can write a note every agent may read.

## Live task state

When an agent calls `ligus_task_update` during its turn, a card appears on
the chat (above the composer) with its status (working, blocked, waiting,
done, stopped), a short summary, a progress bar when it gives one, its open
questions and what it has so far. Each update replaces the agent's previous
one. Other agents on the chat see the current cards in their context (on
the desktop) or read them with `ligus_task_state` (on every host).

A card stays current for two hours after its last update (an agent can ask
for up to 24 hours), then it is hidden. Only the agent itself, or the host
it runs on, can write its card, and each writer has its own card: one host
can't overwrite another's, so an agent that moves to another host (or gets
its own signing key) shows a fresh card while the old one expires.

## On the phone

The phone app reads memory and task state through `ligus serve`
(`GET /v1/memory`, `GET /v1/memory/{id}`, `GET /v1/memory/proposals`,
`GET /v1/chats/{id}/task-state`) and can propose a note or a change with the
`proposeMemory` phone action, signed by the phone's own key as you. Accepting
proposals and editing notes stay on the desktop. See the
[serve API](../../operate/reference/serve-api.md#memory).

## From the command line

`ligus memory list | search | show | add | edit | rm | proposals | accept |
reject` and `ligus task-state <chat-id>`; see the
[CLI reference](../reference/cli.md#memory).

## Telemetry

The [facts export](../concepts/security.md#facts) carries counts only: how
many notes, removals, open proposals and conflicted notes a workspace has,
and how many task-state cards a chat has. Never a title, note, tag, reason,
question or result.

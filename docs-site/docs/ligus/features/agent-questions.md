# Questions from agents

When an agent needs one word from a person to carry on ("Postgres or
SQLite?", "Delete the generated fixtures too?"), it asks with a **decision
card**. Its turn pauses until the card is settled or its time runs out.
Agents ask with `ligus_ask_person` or `ligus_ask_room` (see
[Ligus tools for agents](../addons/ligus-mcp.md#asks)).

## The card

```
 builder asks you                                        11:02 AM
 "Should the cold-day floor apply to swarm checks too, or only to
  routine inspections?"
 [ Swarm checks too ]  [ Routine only ]  Reply…           Not now
 builder decides what to do next. Your answer grants no new permission.
```

- **The header is Ligus's**: who asks, and whom: "asks you", "asks
  Admins", "asks the room", and "needs 2 of you" when the agent wants a
  quorum. Tags say where a card came from: "via sub-agent", or "outside
  Ligus" for a prompt from a Claude Code session Ligus didn't start.
- **The quote is the agent's words**, shown as plain text. Markdown is not
  rendered and links are not clickable; you can still select and copy a
  URL. This holds for questions from the older `ligus_ask_human` too.
- **Options** are buttons. **Reply…** opens a one-line field when the agent
  allows a free-text answer. **Not now** says no to a card put to you
  alone; on a card for the room it only means you abstain, and the others
  can still answer. On an older question, Not now just hides it on this
  device.
- **The effect line**, below the buttons, says in Ligus's words what your
  answer does: for a question, nothing by itself. The agent goes on within
  what it may already do; anything that needs approval is a proposal.
- **Keys**: with the card focused, **1** to **5** pick an option and
  **Enter** sends it.

A card for someone else shows read-only ("Waiting on Maya", "Waiting on
Admins · 1 of 2"). Once settled it shrinks to one line in the transcript
("Theo answered On disk · 10:49 AM"); hover it to see which device signed
the answer. A card whose agent stopped waiting says so, and its buttons go.

## Permission requests

When an agent asks to run something above its tier (an ACP permission
request), the card is a **grant**:

- "Allow once lets builder run this once on MacBook Pro:", then the call's
  own content (the command, or the files and their new content) in a
  scrolling box, then "builder describes it as: …" for the agent's own,
  unverified, title.
- **Allow once** needs a 2-second hold, so it isn't granted by a stray
  click. When the chat is in lockdown, dearth mode is on, the call reaches
  outside the agent's sandbox, or the request came from outside Ligus, it
  asks for your passkey instead.
- Only the host's person or the agent's owner can grant one.
- A call too large to show on a card gets no Allow at all: "Too large to
  review here. Answer at the terminal."

## Where cards show

- In the chat, where the agent asked.
- In the [Review pane](right-rail.md), at the top, while they are open.
- In the [Inbox](inbox.md): one row per card you can answer ("builder asks
  you", with the chat's name). Opening the row opens the card in its chat,
  so you answer with the conversation on screen. Cards for others appear in
  the Inbox only when nothing else needs you.

## A Personal workspace for outside prompts

Permission prompts from Claude Code sessions Ligus didn't start
(`ligus hook permission`) can show commands, paths and file content, and
every chat is visible to every member of its workspace. Send them to a
workspace only you are in: **Add workspace → Make a Personal workspace**
creates one.

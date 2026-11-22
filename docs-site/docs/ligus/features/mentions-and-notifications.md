# Mentions & notifications

Ligus tells you when you're wanted without making you scan every chat.
An `@`-mention that names **you** raises a desktop notification (when the
Ligus window isn't in front) and lights up the channel — and the chat — in the sidebar until you read it.

> This page is about being *notified*. For the full routing table —
> which mention wakes an agent, the Primary Runtime, or a role group —
> see [Multi-agent collaboration → Mentions](multi-agent.md#mentions).

## What counts as a mention of *you*

A message pings you when it isn't yours and it carries any of:

| Mention | Reaches you because… |
|---|---|
| `@all` | the human broadcast: every member |
| `@you` in an **agent's** reply | you are the person the reply answers (see below) |
| `@yourname` / `@name#N` | it names you by handle (the `#N` disambiguates duplicates) |
| `@owners` `@admins` `@contributors` `@viewers` | it names the governance role you hold |

### `@you` means the person who asked

When an agent writes `@you`, it means the **requester**: the person whose
message started the turn. Ligus follows the reply back to the message it
answers, through any hand-offs between agents, to the first message a
person wrote; for a workflow stage that is the person who started the run.
Only that person is pinged — on the desktop, in the sidebar, and on their
phone. Everyone else in the chat sees the reply but gets no mention.

- In the chat, an agent's `@you` is shown as `@<requester's name>` with
  mention styling, highlighted as "mentions you" only for the requester.
  The message itself still says `@you`: signed messages are never
  rewritten, only displayed differently.
- If no person started the chain (an agent started it, or a reply from
  before Ligus 0.1 has no link to what it answered), `@you` reaches nobody.
  It never falls back to everyone.
- `@you` in a **person's** message means nothing: it doesn't ping you or
  anyone else.
- Alone in a workspace, you are always the requester, so an agent's `@you`
  reaches you exactly as before.

Each agent's instructions name the requester for the turn ("You are
answering Ana (@Ana)…") and tell it to use `@you` or their handle to
address them, and `@all` only when every person must act.

### `@all` from an agent

`@all` is the only broadcast; there is no `@here` or `@everyone`. A
person's `@all` always reaches everyone. An agent's `@all` reaches
everyone too, but at most **once every 10 minutes per chat**: a further
agent `@all` in the same chat inside that window reaches only its
requester. The rule is worked out from the chat itself, so your desktop,
your phone's host and every other member's devices agree on it without
coordinating. It caps the noise an agent can make without asking anyone
to configure which agents may broadcast.

Agent mentions (`@agent-name`) never ping a human — they route a turn
to that agent. An *unanswered* agent mention is **queued work**, shown
in the [Review pane](right-rail.md), not as a personal highlight.

## Desktop notifications

The desktop notifies you only about things that need **you**, and stays
quiet about everything else:

| Notification | When |
|---|---|
| *"Name mentioned you"* | a message you didn't write mentions you (see above) |
| *builder asked you something in “Chat”* | an agent stopped in a run **you** asked for to ask a question |
| *builder finished in “Chat”* | a run you asked for finished, on this device or on another (a worker, a box) |
| *builder's run failed in “Chat”* | a run you asked for failed |
| *"A proposal waits on your vote"* | the review queue needs your vote (at most hourly, see [Flow](flow.md)) |
| *"Sam is using scout"* | someone else used an agent of yours, only if you turned on [Notify me](agent-access.md#notify-me-and-usage) for it |

"A run you asked for" means the reply answers a message you wrote, through
any hand-offs between agents. Other people's runs, replies in chats you only
read, and stopped runs never notify.

- **Nothing while you're looking.** When the Ligus window is in front, no
  system notification is shown: the chat row's state (Working, Needs you,
  Done, Failed) already says it.
- **Bursts become one.** Notifications that arrive within a second or two
  of each other are shown as one, for example *"3 chats finished"* or
  *"1 needs you · 2 finished"*. A chat that changes twice in a burst
  counts once.
- Each message notifies once, whether it was typed locally or arrived over
  sync. A reply that arrives more than ten minutes late (a first sync, a
  long time offline) shows in the chat without a notification.

### Messages in the window

Short confirmations and errors inside the window appear one at a time, at
the top right, clear of the message box. If several arrive together, the
rest wait their turn ("2 more"), and repeats of the same message show once.
Errors stay available after they fade: open the command palette (⌘K) and
choose **Recent errors**.

## On your phone

With a `ligus serve` host set up for the phone app, a mention of you (and a
workflow waiting at an approval gate) can also reach your phone as a push
notification. The same rules apply: an agent's `@you` pushes only to the
requester's phone, and the phone shows it as `@<name>` too.

The phone also tells you when an agent **finished replying to you** ("coder
replied"). That goes only to the requester — the person whose message
started the turn, through any hand-offs, or who started the workflow. In a
shared workspace, other members get nothing for a reply that isn't theirs
unless it mentions them, and then it's the one mention notification. A
reply with no person behind it (a scheduled summary, a chain an agent
started) notifies nobody. Alone in a workspace, every reply is yours, so
nothing changes. The desktop notifies the same person when a run they asked
for finishes (see above). Previews are plain text, cut to a short line, and each phone
can turn previews off. See [Ligus on your phone](phone-app.md).

## Sidebar highlights

The same mention lights up the sidebar so it's still there after the
notification banner fades:

- the **channel name** turns bold with a small dot, and
- the specific **chat row** shows the unread dot.

The channel dot lives on the header, so it's visible **even when the
channel is collapsed** and its chats are hidden. A channel lights up if
any chat under it — its main conversation or a focused child chat — holds
an unread mention.

## Reading clears it

Opening the chat marks it read and clears the highlight; any new message
that arrives while you're looking at it stays read. There's nothing to
dismiss manually.

**Read state is per device.** Whether *you've* seen a mention is local
UI state — it lives on your machine, never on the synced workspace log.
Each of your devices tracks its own read cursor, and other members' apps
highlight (or don't) entirely independently of yours. Nothing about what
you've read is shared with the workspace or the relay.

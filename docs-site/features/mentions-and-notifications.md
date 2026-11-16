# Mentions & notifications

Ligus tells you when you're wanted without making you scan every chat.
An `@`-mention that names **you** raises a desktop notification and
lights up the channel — and the chat — in the sidebar until you read it.

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
  before Ligus 2.0 has no link to what it answered), `@you` reaches nobody.
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

## Desktop notification

When a message you didn't write mentions you, Ligus raises one system
notification — "*Name* mentioned you" — per message. Whether the app is
focused or in the background, and whether the message was typed locally
or arrived over sync, the trigger is the same.

## On your phone

With a `ligus serve` host set up for the phone app, a mention of you (and a
workflow waiting at an approval gate) can also reach your phone as a push
notification. The same rules apply: an agent's `@you` pushes only to the
requester's phone, and the phone shows it as `@<name>` too. Previews are plain text, cut to a short line, and each phone
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

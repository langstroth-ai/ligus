# Workspace guards

Ligus keeps a workspace inside safe bounds with a few guards that work the
same whatever runtime your agents use. You find them in **Settings → Guards**.
The names come from beekeeping: a colony posts more guards and narrows its
entrance in a dearth.

## Dearth mode

Dearth mode is a workspace-wide **tight entrance** for hostile periods: an
invite link that leaked, an untrusted repository being processed, a new worker
host you're not sure of yet. An Admin or Owner switches it on with a short
reason; every member then sees a banner at the top of the window until it is
switched off.

While it is on:

- **New members need two Admins.** Joining with an invite on your own is
  suspended. An Admin adds the person, and a second, different Admin approves
  them under *Waiting for a second approval*. Until then they are not a
  member on any device.
- **New devices need a second approval.** A device a member certifies for
  themselves (say a new `ligus serve` box) only counts once an Admin of
  another account approves it. Until then every device holds its events, in
  every identity mode, along with any event no certified device vouches for;
  held events apply once the device is approved. Devices already trusted when
  dearth mode began are unaffected.
- **Agents reach only allow-listed MCP servers.** Everything else is closed:
  no web fetch or search, no shell, no MCP server outside the list. Runtimes
  Ligus can't confine from outside (aider, pi, Hermes, Antigravity, Copilot)
  are refused with a note in the chat. See
  [the sandbox envelope](../concepts/security.md#the-sandbox-envelope-around-every-agent-turn).
- **Protected (brood) paths need a human.** A proposal that changes a
  protected path applies only after a human member approves it themself, even
  if its quorum is met. The gate reads the paths in the diff itself, not the
  file list the proposal declares, and matches case-insensitively (on macOS
  and Windows `.GitHub/` is `.github/`). If the patch changes after the
  approval, the approval no longer counts. That includes an alternative in a proposal set: its
  pre-flight and apply wait for the approval, and a low-bar path zone can't
  lower its review bar. The default list covers CI and deployment config
  (`.github/**`, `.gitlab-ci.yml`, `.circleci/**`, `Jenkinsfile`, `deploy/**`,
  `infra/**`, ...); edit it as globs, one per line.

The **Entrances** list shows every way outside content or traffic reaches an
agent in this workspace and what dearth mode does to each.

Every one of these rules is checked again on every device when the change
syncs in, so a member who edits their own client can't switch dearth mode
off, approve their own join, or vote as someone else: those events are
dropped everywhere.

Switching dearth mode off returns to normal rules, and anyone still waiting
for a second approval is admitted.

## Chat lockdown

A chat locks itself down when one of its agent turns shows a sign of
infection:

- a **prompt-injection marker** in something an agent read: a tool or MCP
  result telling it to ignore its instructions, pretend to be a system
  message, pipe a download into a shell, or send credentials somewhere;
- a **credential in an agent's changes**: an API key, token or private key in
  the lines a proposed diff adds;
- an **unrequested CI change**: the agent edited CI configuration
  (`.github/workflows`, `.gitlab-ci.yml`, `Jenkinsfile`, ...) when the request
  didn't mention CI.

A locked chat shows a red bar under its header and a note in the
transcript saying what tripped it. Until a member clears it:

- agents in that chat have **no network or MCP access** at all, and runtimes
  Ligus can't confine are refused;
- **every** proposal from that chat needs a human member's own approval before
  it applies, whatever its quorum.

Only a **human member** (Contributor or above) can clear a lockdown, with
**Clear lockdown** on the bar. A non-member, a Viewer, or an agent's account
clearing it is refused on every device. A clear lifts exactly the signals the
bar showed: one raised after you looked (by another device, or while your
clear was on its way) stays in force until it is cleared too. Anyone may lock
a chat by hand.
The detectors only record what kind of thing they found (a credential type, a
marker name, a count), never the content.

## Hygiene

Agents leave things behind: a turn interrupted by a crash leaves its git
worktree, a deleted chat leaves its checkout, a proposal waits for review
while the branch moves on until its diff no longer applies. A few minutes
after launch and then every six hours, Ligus sweeps the workspace's bound
repository (**Sweep now** in Settings → Guards runs it at once):

- a turn worktree with no turn running is removed; any unfinished edits are
  saved first as a patch under `.ligus/stopped-turns/`. One a turn is still
  using, or one parked for a live chat's next turn
  ([worktree reuse](flow.md#reusing-worktrees)), is never touched;
- the checkout of a deleted or archived chat is removed, along with any
  worktrees parked for it, but its `ligus/chat/...` branch is kept, since it
  may hold applied work nobody has merged yet;
- an open proposal whose diff no longer applies to its chat's branch is
  marked **stale** and taken out of the Review inbox (you can still restore
  it), instead of sitting there forever.

## Plan windows: staggered throttles

Subscription runtimes (Claude Code, Codex, Copilot, Antigravity) have usage
windows, such as Claude's five-hour and weekly limits. Instead of every agent
running at full speed until the window is exhausted and then all stopping at
once, Ligus backs off in steps:

- **Your own requests always run.** A message you send, a regenerate, a
  proposal you act on: never throttled.
- **Scheduled runs pause first**, from about 70 % of the window. A due
  schedule simply waits and fires once there's room again.
- **Workflow stages and agent-to-agent hand-offs pause next**, from about
  85 %. A workflow stage waits (the run card keeps showing it running; Cancel
  still works); a hand-off between agents stops with a note in the chat, and
  mentioning the agent again continues it.
- **Each agent has its own threshold**, a few percent either side of those
  figures, so agents stop one at a time as the window fills.

The 15 % between the workflow threshold and the limit is slack kept for your
interactive turns. It is deliberately not larger: idle headroom just invites
background agents to fill it with busywork.

Ligus learns where a window stands from the runtime itself: Claude Code
reports rate-limit events while it works (status, how full, when it resets),
and any subscription CLI that fails with a usage-limit error marks its window
full until it resets (an hour, when the CLI doesn't say). **Plan windows** in
Settings → Guards shows each runtime's window and anything being held back.
A runtime that reports nothing is never throttled.

## Long agent loops: the patch-leaving rule

When an agent
works through Ligus's own tools (the built-in file tools and MCP, used by API
and local models), Ligus measures its progress instead of taking its word for
it: a write that actually changes a file, or a test run with more tests
passing than before in the turn. After **12 tool calls in a row with no
measurable progress**, every further call is answered with a check-in: the
agent stops, says what it tried and what is blocking it, and asks you how to
continue. Set `LIGUS_PATCH_LEAVING_CALLS` to change the number (0 turns the
rule off). Subscription CLIs (Claude Code, Codex, ...) run their own tool
loops and are not affected.

## Growing the workspace when it's ready

A workspace starts with one agent on your device, and a
small card above the chat suggests growing it only when the workspace's
occupancy says so:

- **Add an always-on host** when requests keep waiting for an agent whose
  machine is offline (a `ligus serve` box answers while laptops sleep);
- **Add a worker with its own capacity** when agents are pausing near a plan
  window's limit;
- **Add another agent** when one agent wrote 80 % or more of the last forty or
  more agent replies.

Dismiss a suggestion and it stays dismissed on this device.

## What the guards record

Each device keeps a small local log of what its guards did: lease heartbeats
sent and received, ingest decisions on synced events (every rejection, and a
sample of acceptances), the worker's tool role gate, MCP tools offered and
dropped, redactions, lockdown signals, worktree clean-ups, throttles and the
progress of long tool loops. It holds only ids, kinds, counts and short
reason codes, never message, prompt or diff text, file paths or repository
names; it is never synced, and it keeps the newest 20,000 entries. It lives
in the `guard_signals` table of the device's `ligus.db`, for local monitoring.

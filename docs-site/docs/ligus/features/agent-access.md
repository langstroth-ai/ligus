# Who can use your agents

An agent's turn runs on its owner's device, or on the worker or serve box it
is hosted on, **on that owner's credentials**: their API key, their Claude
Code, Codex or Copilot subscription, their local model. So the owner decides
who may start those turns, and the device that would run a turn checks
before it starts. This page shows the settings; the rules behind them are in
[Agents → Who can use an agent, and who pays](../concepts/agents.md#who-can-use-an-agent).

## Where the settings are

Open the right rail's **Tools** pane and switch it to **Workspace** (or click
**Edit in Workspace**). Each agent in **Agent roster** has its access
settings at the bottom of its card:

- **Who can use this agent**: **Only me** or **Everyone in the workspace**.
- **Turns per member per day**: **Default**, **Unlimited** or **Custom…**.
- **Daily cap (USD, optional)**: an estimated spending cap per day.
- **Notify me when someone else uses this agent**.
- **Usage on this device**: who used the agent today and this month.

Change them and press **Save access**. Only the agent's owner sees these
controls. Everyone else sees a small badge on the card instead: **Only its
owner**, **Everyone** or **Admins only**.

## Who can use an agent

| Agent | Default | Options | Who can change it |
|---|---|---|---|
| Personal (you added it, so you own it) | **Only me** | **Only me**, **Everyone in the workspace** | you, and only you |
| Shared (it has no owner) | everyone who can post | **Admins only**, **Everyone in the workspace** | a workspace Admin or Owner |

**Only me** means exactly that: no one else can use the agent, whatever their
role. A workspace Owner or Admin can't use your personal agent unless you open
it, and can't change the setting for you. Every device in the workspace drops
a change to these settings made by anyone but the owner (an Admin, for a
shared agent).

**Opening an agent asks for your passkey.** Switching it to **Everyone in the
workspace**, or adding an agent that is already open, is one of the changes
that need your passkey, like moving an agent to another host or runtime.
Narrowing it back to **Only me**, changing its budgets and turning the
notice on or off don't. See [Passkeys](../concepts/security.md#passkeys).

A turn runs only if **every person behind it** may use the agent: whoever
wrote the message, and the person who started a chain when one agent
mentions or hands off to another. Asking your own agent to ask someone
else's doesn't get around it. A chain that can't be traced back to a person
runs only on an agent open to everyone.

When you type `@`, agents you can't use stay in the list, greyed out and
marked **Not available**, with the reason (for example *"Only its owner can
use this agent"*), and you can't pick them.

## Daily limits and a spending cap

Both reset at 00:00 UTC.

| Setting | Default | What it does |
|---|---|---|
| Turns per member per day | 50 on a personal agent; no limit on a shared one | Limits how many turns each other member can start each day. **Unlimited** removes the limit; **Custom…** takes a whole number of at least 1. |
| Daily cap (USD) | no cap | Once the day's estimated spend on this agent reaches it, other members' turns are refused. Everyone's turns count towards it, yours included. |

Your own turns on your own agent are never limited. A turn counts against the
person who asked. When an agent's turn involves several people (one member's
agent asking yours), every one of them other than you must be under the
limit.

Spend is estimated from the token usage the runtime reports. Most
subscription CLIs and local models report none, so their turns count towards
the turn limit but not the dollar cap. The cap is checked before a turn
starts, so a turn already running finishes even if it goes past the cap.

## What others see when they're refused

A refused turn never reaches a model and costs nothing. The device that
would have run it answers the message with one short reply in the agent's
name, which no model wrote:

| Why | Reply |
|---|---|
| Not open to them (personal agent) | *"scout is only available to its owner."* |
| Not open to them (shared agent restricted to Admins) | *"scout is only available to workspace Admins."* |
| Over the per-member limit | *"scout reached today's limit of 50 turns per member (resets at 00:00 UTC)."* |
| Over the daily cap | *"scout reached its daily spending cap of $5.00 (resets at 00:00 UTC; its owner's own turns still run)."* |

A member gets at most **one such reply per agent every five minutes**. Further
refusals in that window are silent: the turn is still refused, just without
another note. A reply never says more than the agent's settings already
show: not its prompt, its runtime or how much of its owner's plan is used.

If your teammate's laptop is closed, your own device would normally answer
a waiting turn itself after a short wait. For an agent you may not use, it
refuses in the same way instead, so someone else's personal agent never runs
on your device.

## Notify me and usage

With **Notify me when someone else uses this agent** on, your desktop shows
a notification (*"Sam is using scout"*, or *"Sam used scout"* when the turn ran elsewhere) whenever another member's turn
runs on the agent, whether it ran on that desktop or on a box (you see it
when the reply syncs). The notice is for personal agents.

**Usage on this device** lists, for each member, turns today, turns this
month and the estimated cost this month (or *not reported*). Usage is counted
by the device or box that runs the agent and never synced, so an agent that
runs on two machines is counted on each separately.

Another member's direct mention of your agent also counts as background
work on your subscription: when its plan window is about 70% used, their
turns wait first and yours keep running.

## `@ligus`, the built-in assistant

`@ligus` isn't a workspace agent. It runs on the device of the person who
created the chat, on that person's account. So in a chat with other people
it answers **only that person** by default. Anyone else who mentions it
gets:

> *Ligus in this chat runs on another member's device and account, and only
> answers them. Mention one of your own agents instead, or ask them to let
> members use it.*

To let others use `@ligus` on your device, open **Settings → Profile →
Privacy** and turn on **Let members use @ligus on this device**. It is off by
default and applies to this device only (it isn't synced). In a workspace
where you are the only person, `@ligus` always answers you.

## Agents on a worker or serve box

An agent hosted on a [worker or serve box](../../operate/concepts/headless-agents.md#who-may-use-a-hosted-agent)
keeps its owner's settings. The box checks who may use the agent and the
daily limits before it takes a turn from its queue, refuses with the same
short reply, and counts usage in its own database. The owner can read a
box's counts from the box (`GET /v1/agents/{id}/usage`, owner only). A box
has no plan-window throttle.

## Roles, workflows and the phone

- **Roles.** Workspace roles (Owner, Admin, Contributor, Viewer) don't
  override **Only me**: only the owner decides who uses a personal agent.
  Roles matter for shared agents (Admins and Owners change their setting,
  and **Admins only** lets only them use it) and for what a turn may do once
  it runs: the [author gate](../concepts/security.md#author-gate) still
  limits a lower-role member's turns to read-only tools.
- **Workflows.** A workflow stage that uses an agent runs only if the person
  who started the run may use that agent and is within its daily limit. A stage run by `@ligus` follows the `@ligus` rule above.
- **The phone app.** Your phone acts as you, so it can use the agents you
  can. The box tells the phone which agents you may use, and a message to one
  you can't use gets the same short reply. Access settings are changed on
  the desktop.

## Usage reports to a model gateway

When agent turns go through a [model gateway](../../operate/deploy/model-gateways.md#attribution-headers)
with attribution turned on, each request carries an `X-Ligus-Member` header
naming the person who asked, without revealing who they are. It is a keyed
hash of that person's account id and the workspace id (HMAC-SHA256,
truncated to 32 hex characters), so the gateway can count, rate-limit and
budget per person, but can't recover their name, account or anything else.
The same person gets a different value in each workspace, so their use can't
be linked across workspaces. The header is left out when no person could be
traced, and no message content is sent in headers.

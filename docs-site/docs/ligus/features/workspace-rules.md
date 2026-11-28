# Workspace rules

A workspace's **rules** decide what every device accepts on it. New
workspaces start on **rules 4**. A workspace made with an older Ligus stays on
the rules it has until its Owner upgrades it, from **Settings → Workspace
rules** on the desktop or with `ligus workspace rules upgrade`.

## What rules 4 changes

- **Each agent carries its own model choice.** Changing the model of your own
  agent within the same source (another model from the same gateway, say)
  needs no passkey. Moving it, or changing what answers, takes one.
- **An agent runs only on a host that offers what it runs.** Placing your own
  agent on your own computer or worker is yours to do, with a passkey; placing
  an agent anywhere else, and changing a shared agent (one with no owner), is
  an Admin's.
- **Model server addresses stay on the hosts that run them.** An agent whose
  runtime keeps a server address on the workspace keeps working, but that
  address can't be changed, and no new one can be added (see
  [Model server addresses](#model-server-addresses)).
- **New members need an account**, and a chat can't carry its own copy of an
  agent: every chat runs the workspace's agent (see
  [An old copy of an agent in a chat](#old-copy)).
- **Devices on Ligus before 0.5 go read-only** on the workspace: they still
  read it, but can't change anything in it until they're updated.
- **There is no going back** to the older rules.

The full list of checks is in the
[threat model](../../operate/security/threat-model.md#rules-4).

## Upgrading an existing workspace {#upgrade}

Only the workspace's **Owner** upgrades it. **Settings → Workspace rules**
shows **Upgrade workspace rules** with:

- **The readiness list**, one line each:
    - devices whose last report says they run Ligus before 0.5, by name and
      build: "lt on zaz-mbp (Ana) is on 0.4.1: it goes read-only here until
      it's updated";
    - devices that haven't said which Ligus they run (older builds don't):
      any of them before 0.5 goes read-only too;
    - agents placed on a host that wouldn't count under rules 4 (removed,
      revoked, or a host record that isn't its own device's): choose where
      they run again;
    - members with no account (they keep working);
    - runtimes that keep a model server address on the workspace.

  The list informs: you can upgrade anyway. Update the old devices first if
  you want them to keep working here.
- **What changes**, behind a disclosure.
- One button, **Upgrade workspace rules**. It asks for your passkey once, then
  every device applies rules 4 from that point on. Events before it are never
  judged again.

Your account needs a passkey and a recovery key first (Settings → Passkeys &
keys).

On the command line:

```sh
ligus workspace rules                     # the version, and the readiness list
ligus workspace rules upgrade --dry-run   # the list, nothing written
ligus workspace rules upgrade             # asks your desktop or phone for the passkey
ligus workspace rules upgrade --no-wait   # exit 4; finish with ligus handoff wait, then run it again
```

A terminal has no passkey of its own: the switch goes to your desktop or phone
to confirm (see [Steps that need your passkey](../reference/cli.md#handoff)),
or `--recovery-words-file PATH` confirms it with your recovery key.

## Pinning agents to what they run {#pin}

On rules 4, an agent made before the upgrade may still name a runtime (a
preset with a model inside it). **Settings → Workspace rules** lists them as
"Pin @coder to Claude Code / opus": **Pin** makes the agent carry that source
and model itself, so changing its model later needs no passkey. Each pin asks
for your passkey once (it changes what the agent names). You see your own
agents; an Admin also sees everyone else's. **Change model…** in an agent's
**⋯** menu pins it the same way.

## Changing an agent's model or where it runs {#change}

Each agent's **⋯** menu (Tools pane) has **Change model…** and **Runs on…**,
the same form as **Add an agent** with only that one field:

- **Change model…** lists what the agent's source offers where it runs: a
  gateway's own list from its host (with `auto` when the gateway picks the
  model itself), a coding CLI's models, a model server's.
- **Runs on…** lists your own devices and the hosts that offer the agent's
  source, each with its state ("ready", "signed in", "offline"), and one line
  saying what it means: "Turns run on build-1. When it's offline, @coder
  waits." A host that doesn't offer the source can't be picked.

What it asks for depends on the rules:

| | Rules 4 | Rules 3 |
|---|---|---|
| Your agent, another model from the same source | no passkey | an Admin's, with a passkey |
| Your agent, to your own device or worker | one passkey | an Admin's, with a passkey |
| Your agent, to someone else's host or a box you don't own | an Admin's, one passkey | an Admin's, with a passkey |
| A shared agent | an Admin's | an Admin's, with a passkey |

On rules 3 the model belongs to the agent's runtime: the form reuses a runtime
that runs the new model, or defines one first (then it asks **Confirm 1 of
2**, **2 of 2**). In `lt` the agent's menu has the same two items, and on the
command line it is `ligus agents set <agent> --model M` or
`--runs-on here|HOST`.

## Model server addresses {#model-server-addresses}

A runtime made before rules 4 may keep a model server's address (an Ollama or
OpenAI-compatible URL) on the workspace, where every member can read it. The
agent that uses it shows one quiet line in the Tools pane: "Its model server's
address is visible to every member." On rules 4, on the computer that runs the
agent, **Move it to this computer** puts the address in this computer's
`host.toml` (it never leaves it), offers that server by name, and points your
agents that use it at that name, each with a passkey. A box lists the server
in its own `host.toml` instead (see
[What a host offers](../reference/cli.md#what-a-host-offers)). The old runtime
stays until an Admin removes it in Settings → Models & runtimes.

## An old copy of an agent in a chat {#old-copy}

Ligus before 0.5 could write an agent's record into one chat, which then ran
that copy instead of the workspace's agent. On rules 4 every chat runs the
workspace's agent, and new copies are refused. An Admin opening a chat that
still has one sees "This chat has an old copy of @x." with **Use the
workspace's @x**, which only hides the notice on this device: nothing is
written, and the chat already uses the workspace's agent.

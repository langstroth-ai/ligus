# The community relay

`https://relay.langstroth.ai` is a relay the Langstroth project runs for
anyone to use. Ligus offers it as the first choice when you set up sync
(onboarding, **Add workspace → Create** with no relay set, and **Use the
community relay** under **Settings → Team sync**). Ligus never switches to it
on its own: it is used only when you pick it.

## What you get

- **Free**, with nothing to set up.
- **Best-effort.** There is no uptime guarantee and no support commitment. It
  may be slow, unavailable, or rate limited at times.
- **It can be reset.** Stored envelopes may be cleared, with notice given in
  [the relay's discussions](https://github.com/langstroth-ai/langstroth-relay/discussions).
  Every device keeps its own full copy of a workspace, so a reset loses no
  history; devices push again when they next sync.

## What its operator can see

The relay forwards end-to-end-encrypted envelopes. It never holds a workspace
key, so it can't read messages, files, code or names. Its operator (and
anyone who compromises it) can see:

- the **ciphertext** it stores and forwards, and its **sizes**;
- **timing**: when each device connects and sends;
- **room ids**: the random, opaque id of each workspace (not its name);
- the **network addresses** devices connect from, as any server would.

Never the content. See the [security model](../security/index.md) for what
each component holds.

## When to self-host

Use the community relay to try Ligus or for low-stakes work. For anything
important (a team that depends on sync, uptime you need, or metadata you'd
rather nobody else holds) [run your own relay](self-host.md). Moving later is
easy: set the new relay in **Settings → Team sync** on each device, then send
new invites.

## The URL travels in the invite

Only the person who creates a workspace (or the operator who sets up a host)
types a relay URL. From then on it is part of every invite, so people who join
never choose a relay. That holds for the community relay, your own relay, and
the Kubernetes patterns in
[Kubernetes: where the relay runs](../deploy/kubernetes.md#where-the-relay-runs).

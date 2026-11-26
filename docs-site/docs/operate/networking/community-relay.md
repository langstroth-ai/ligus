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

## Membership on the community relay

The community relay runs with [server-side membership](../ops/server-side-membership.md)
on, but it has no sign-in: it trusts only tokens signed with its operator's
membership key, and nobody else can issue those. So for everyone but its
operator a workspace there stays unclaimed, and the People pane offers no
**Claim ownership**. That doesn't open your workspace to anyone: events are
end-to-end encrypted and signed, so a stranger who knows the workspace id
can't read them, pose as a member or change anything; at most they can make
the relay store junk that every device drops. If you want a relay that
refuses writes from non-members, [run your own](self-host.md) with
`LANGSTROTH_RELAY_MEMBERSHIP=1` and an issuer key, and see
[Claiming a workspace](../ops/server-side-membership.md#claiming-a-workspace).

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

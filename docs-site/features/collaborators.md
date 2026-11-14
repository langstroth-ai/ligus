# People, contacts & DMs

Ligus has no user directory, no friend requests and no lookup by username.
You reach people through workspaces you share with them, or by handing them a
link. Open **People** from the button on the left workspace rail.

## People you share a workspace with

People lists every member of every workspace you share with them, once per
account, with:

- their name and picture (names aren't unique and identify nobody);
- their **safety code**, the fingerprint of their account key (see
  [Identity & devices](../concepts/identity.md#safety-code));
- the workspaces you share.

Their devices' encryption keys come from those workspaces' signed logs, not
from a server, so Ligus can seal things to them without asking anyone who
they are. If two of your workspaces disagree about someone's account key,
People shows a warning and Ligus won't send them anything until it is
resolved.

From a person's row you can:

- **Message** them (a direct message, below);
- **Invite to…** another workspace you administer. The invite is sealed to
  their verified devices and dropped into their contact box (below). If it
  can't be delivered, you get a single-use link to send them yourself.

## Direct messages

**Message** opens a private conversation with one co-member. A DM is a
two-person workspace with its own end-to-end encryption key, so the relay
only ever sees ciphertext.

- **Its room and invite are random.** Nothing about a DM is derived from
  either person's account id, a forge id, an email or a name, so nobody (the
  relay included) can work out whether two people have a DM.
- **You found it.** The DM's invite names your account and carries your
  safety code, and it is addressed to the other person's account: it can't be
  used by anyone else.
- **They accept only if it checks out.** Their Ligus compares your safety
  code with the one your shared workspaces register for you. A mismatch is
  refused. From someone you share no workspace with, Ligus asks them to
  compare safety codes with you first.
- **Opening at the same time.** If you both open a DM at the same moment,
  you get two. Both sides keep the older one (the earlier founding time,
  then the smaller room id) and archive the other; the archived one stays
  readable and is linked from the one you keep.

## Contacts (relay v0.3)

Each of your devices keeps a **contact** on its relays: a random id and a
drop box. Ligus publishes a record there that anyone can check (your account
key, the device's certificate, and its signed encryption key), refreshes it
daily, and announces the contact id on what the device writes, so
co-members can deliver to it.

- An invite to you is sealed to your verified devices before it is dropped.
  The relay can't read it, and garbage that doesn't open is discarded.
- New invites show as a banner with the founder's safety code and whether
  it matches what you know. **Accept** joins; **Dismiss** deletes it from the
  relay.
- Only the device that owns a box reads it, and deletes what it handled.

A relay without the contact routes (v0.2) answers "not found". Ligus then
says **your relay doesn't support contacts yet** and falls back to links, QR
codes and co-member invites by link. Nothing else changes.

## Where this differs from workspace invites

| | People (this page) | Workspace invites |
|---|---|---|
| Reach by | a shared workspace | a link you hand over |
| Scope | 1:1 DMs, and inviting co-members | a whole shared workspace and roster |
| Set up in | **People** (left rail) | the **People** pane (right rail) or the ＋ button |

See also: [Identity & devices](../concepts/identity.md),
[Invites & joining](invites.md).

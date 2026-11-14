# Invites & joining

Workspaces are private by default. To let a peer in you share an
**invite** — an opaque code that carries everything they need to
sync: the relay, the room, and the end-to-end-encryption key. No provider
or directory is involved: you hand over a link, or invite someone you
already share a workspace with.

![Invite controls in the People pane](../images/invite-pane.png){ width="800" }

## Create / join a team (the ＋ button)

![Creating a team â the invite code is redacted here](../images/team-invite.png){ width="760" }

The common path is the **workspace rail's ＋ button**, which opens
the **Add workspace** modal with two tabs:

**Create** — name the workspace and Ligus spins up a private,
end-to-end-encrypted team room (it generates the room id and the
E2EE key). You become the owner and immediately get an **invite
code** to share. Anyone who pastes it joins and syncs with you.

**Join** — paste an invite code someone shared with you and click
**Join workspace**. The code carries the relay, room, and key, so
joining is a single paste — nothing else to configure. You show up in
the workspace **roster immediately** on joining — no need to open a
chat first before the rest of the team sees you.

Invite codes look like `hivews1:…` (see the
[invite code reference](../reference/invite-payload.md) for the
format). Because the code embeds the workspace key, treat it like a
secret — share it over a channel you trust.

Only an **Admin** (or the Owner) can make an invite code, and each code
carries a fresh **single-use invite**: it admits one person, once. Copy a
new code for each person you invite. Someone who joins without an unused
invite, or a removed member coming back, is refused on every device until an
Admin invites (or re-adds) them.

The relay-brokered **short code** — a brief token (e.g. `K7P2QX`) that
published the full invite to the relay so a peer could redeem it — has
been **removed**: it put the workspace key on the relay, which E2EE now
forbids (see [Security & trust](../concepts/security.md)). Share the
full `hivews1:` code out-of-band instead, or
[invite a co-member](#invite-someone-you-already-share-a-workspace-with)
for a paste-free path that keeps the key off the relay.

You can **Copy invite** (the full `hivews1:` code) or **Leave** a
workspace from the *Your team workspaces* list.

## Check who sent an invite

Every invite carries its founder's **safety code** (the fingerprint of the
founder's account key, see [Identity & devices](../concepts/identity.md#safety-code)).
When you paste one into **People → Join with an invite**, Ligus shows it and
compares it with what you know:

- **Matches**: the founder is someone you share a workspace with, and the
  code is theirs.
- **Doesn't match**: the invite claims to be from someone you know but
  carries another key. Ligus won't join it.
- **Unknown**: you share no workspace with the founder. Compare the code with
  them out of band before you join. For a direct message, Ligus asks you to
  confirm you did.

A name on an invite proves nothing; the code does.

## Invite someone you already share a workspace with

In **People** (left rail), pick the person and **Invite to…** a workspace you
administer. Ligus:

1. Takes their devices' encryption keys from the signed logs of a workspace
   you share (never from a server), and refuses if your workspaces disagree
   about their account key.
2. Issues a single-use invite on the target workspace's log.
3. **Seals it to their devices** and drops it into their contact box on the
   relay (relay v0.3). It shows up for them as a banner they accept or dismiss.

If they have no contact yet, or the relay doesn't support contacts, you get the
single-use link to send them yourself.

There is no invite by username and no import of a forge organization: Ligus
has no directory to look anyone up in.

## Nearby on LAN (roadmap)

Zero-config nearby discovery on the same network — synthesizing an
invite from a local announcement so peers can join without pasting a
code — is a tracked follow-up and **not in the current build**. See
[LAN discovery](../networking/lan.md). For now, share an invite code
(above) or point both peers at a relay.

## Cross-network sync

Peers on different networks converge through a shared **relay**: set
the same **Relay URL** and join the same room + key (an invite code
does this automatically). Configure the relay under
**Settings → Team sync**. See [Self-hosting a relay](../networking/self-host.md)
for how to run one.

## Revocation

To remove a peer, open the **People** pane and use one of the two
controls on their row:

- **Remove** — takes them off the roster only. Their past envelopes
  stay in history.
- **Remove & revoke** — removes them **and rotates the workspace
  key**, so they can't read any new messages. Use this for a bad
  actor or a lost device.

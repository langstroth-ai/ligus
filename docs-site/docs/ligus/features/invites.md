# Invites & joining

Workspaces are private by default. To let a peer in you share an
**invite** — an opaque code that carries everything they need to
sync: the relay, the room, and the end-to-end-encryption key. No provider
or directory is involved: you hand over a link, or invite someone you
already share a workspace with.

<!-- Screenshot pending (Invite controls in the People pane): images/invite-pane.png. See images/CAPTURE_GUIDE.md; restore the image line once it is captured. -->
## Create / join a team (the ＋ button)

![Creating a team: the invite code is redacted here](../images/team-invite.png){ width="760" }

The common path is the **workspace rail's ＋ button**, which opens
the **Add workspace** modal with two tabs:

**Create** — name the workspace and Ligus spins up a private,
end-to-end-encrypted team room (it generates the room id and the
E2EE key). You become the owner and immediately get an **invite
code** to share. Anyone who pastes it joins and syncs with you. If no
relay is set yet, **Create** asks where the workspace syncs: the
[community relay](../../operate/networking/community-relay.md)
(preselected, best-effort), your own relay's URL, or no relay (this device
only, and nobody can join until you add one). With a relay already set, the
workspace uses it.

**Join** — paste an invite code someone shared with you and click
**Join workspace**. The code carries the relay, room, and key, so
joining is a single paste — nothing else to configure. You show up in
the workspace **roster immediately** on joining — no need to open a
chat first before the rest of the team sees you.

Invite codes look like `hivews1:…` (see the
[invite code reference](../../operate/reference/invite-payload.md) for the
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

An invite needs a relay: a workspace created while **Settings → Team sync →
Relay URL** was empty has none, so Ligus copies no invite for it and offers a
link to Team sync instead. Set the Relay URL there (it applies to the
workspace you're syncing), then copy a new invite.

## Check who sent an invite

Every invite carries its founder's **safety code** (the fingerprint of the
founder's account key, see [Identity & devices](../concepts/identity.md#safety-code)).
When you paste one into **People → Join with an invite**, Ligus shows it and
compares it with what you know:

- **From <name>**: the founder is someone you share a workspace with, the
  code is theirs, and their device signed the invite.
- **Says it's from <name>**: the invite names someone you know and their
  code, but their device didn't sign it (someone else can copy a name and a
  code). It may still be genuine, for example an invite another Admin made;
  join only if you expected it. A direct message invite like this is refused.
- **Doesn't match**: the invite claims to be from someone you know but
  carries another key. Ligus won't join it.
- **Unknown**: you share no workspace with the founder. Compare the code with
  them out of band before you join. For a direct message, Ligus asks you to
  confirm you did.

A name on an invite proves nothing, and nor does a code on its own: the
founder's signature ties the two to the invite.

On the phone, **People → Open an invite link** (paste or scan the QR code)
shows the same labels, read by your home host. A host serves one workspace,
so from the phone you can join a direct message invite; a workspace invite is
for a desktop. See [Ligus on your phone](phone-app.md#safety-codes-invites-and-direct-messages).

## Invite someone you already share a workspace with

In **People** (left rail), pick the person and **Invite to…** a workspace you
administer. Ligus:

1. Takes their devices' encryption keys from the signed logs of a workspace
   you share (never from a server). If your workspaces disagree about their
   account key, the earliest-pinned one stands and the others are marked as
   disputing them; Ligus refuses only when every workspace that registers a
   key for them disputes it.
2. Issues a single-use invite on the target workspace's log.
3. **Seals it to their devices** and drops it into their contact box on the
   relay (relay v0.3). It shows up in their [Inbox](inbox.md), where they accept or dismiss it.

If they have no contact yet, or the relay doesn't support contacts, you get the
single-use link to send them yourself.

There is no invite by username and no import of a forge organization: Ligus
has no directory to look anyone up in.

## Nearby on LAN (roadmap)

Zero-config nearby discovery on the same network — synthesizing an
invite from a local announcement so peers can join without pasting a
code — is a tracked follow-up and **not in the current build**. See
[LAN discovery](../../operate/networking/lan.md). For now, share an invite code
(above) or point both peers at a relay.

## Cross-network sync

Peers on different networks converge through a shared **relay**: set
the same **Relay URL** and join the same room + key (an invite code
does this automatically). Configure the relay under
**Settings → Team sync**. See [Self-hosting a relay](../../operate/networking/self-host.md)
for how to run one.

## Revocation

To remove a peer, open the **People** pane and use one of the two
controls on their row:

- **Remove** — takes them off the roster only. Their past envelopes
  stay in history.
- **Remove & revoke** — removes them **and rotates the workspace
  key**, so they can't read any new messages. Use this for a bad
  actor or a lost device.

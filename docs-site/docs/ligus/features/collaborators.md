# People, contacts & DMs

Ligus has no user directory, no friend requests and no lookup by username.
You reach people through workspaces you share with them, or by handing them a
link. Open **People** from the button on the left workspace rail.

## People you share a workspace with

People lists every member of every workspace you share with them, once per
account, with:

- their name and picture (names aren't unique and identify nobody). Names
  are shown without the invisible and text-direction characters that can
  make one name read as another, and a **Look-alike name** tag marks two
  people whose names look the same: tell them apart by safety code;
- their **safety code**, the fingerprint of their account key (see
  [Identity & devices](../concepts/identity.md#safety-code));
- the workspaces you share.

Their devices' encryption keys come from those workspaces' signed logs, not
from a server, so Ligus can seal things to them without asking anyone who
they are. Whoever founds a workspace can add any account there with a key of
their own, so one workspace never overrides the others: Ligus trusts the key
from a workspace that person founded, else from the workspace you joined
first that registers one, and ignores a workspace that says otherwise about
them. Only when every workspace that registers a key for someone disputes it
does People show a warning, and Ligus sends them nothing until it is
resolved. When you leave a workspace, it stops vouching for anyone.

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
- **You found it.** The DM's invite names your account, carries your safety
  code and is signed by your device, and it is addressed to the other
  person's account: it can't be used by anyone else.
- **They accept only if it checks out.** Their Ligus checks your device's
  signature and compares your safety code with the one your shared
  workspaces register for you. An unsigned invite or a mismatch is refused.
  From someone you share no workspace with, Ligus asks them to compare safety
  codes with you first.
- **Nothing is sent until your founding arrives.** Until the DM's signed
  founding reaches their device, they see **Waiting for <your name>'s
  founding to sync** and Send is off. It clears on its own once it syncs.
- **Opening at the same time.** If you both open a DM at the same moment,
  you get two. Both sides keep the older one (the earlier founding time,
  then the smaller room id) and archive the other; the archived one stays
  readable and is linked from the one you keep.

### When their device isn't reachable yet

The DM's invite is dropped into the other person's contact box (below). Right
after someone joins a workspace you share, your device often knows their keys
but not their contact id yet, so the drop can't happen. Ligus then **holds the
invite** and delivers it on its own once their contact id reaches you (it
rides their next sync of a workspace you share). The DM list shows **Your
invite will be delivered when <name>'s device is reachable**, with **Share
link** to send it yourself meanwhile and **Stop waiting**. Once delivered,
the line says they got it.

- **Sealed at rest.** The held invite is sealed to your device's own
  key-agreement key and kept in the device's secret store. The list of held
  invites (`held-dm-invites.json` in the data dir) holds no secret.
- **Sent once.** Each held invite is marked as sending on disk before it is
  dropped, so a second pass, a restart or a crash never sends it again. If
  they already joined from a link you shared yourself, it is never sent.
- **Their keys are checked again.** At delivery the invite goes only to
  device keys their account signed on your shared workspaces' logs, and only
  to devices you had when you opened the DM, under the same account key. If
  their account key changed, the workspaces you share disagree about it, or a
  device key appeared that you didn't have then, nothing is sent: the DM
  list shows the unverified-recipient warning. Compare safety codes, then
  **Deliver anyway** (to their current verified devices) or share the link.
  Deliver anyway is tied to the safety code it showed you: if their key
  changes again before you click, it is refused and you compare again. A
  device revoked since, even during a delivery pass, is left out.
- **Bounded.** At most 20 invites wait at once, and each waits 7 days. An
  invite link has no expiry of its own (it is single-use), so the 7 days are
  the delivery's: after that the held copy is forgotten and you send the link
  yourself if you kept it, or they open a DM with you.

The desktop and `ligus serve` run the same code for this. On a box the phone
sees `pending` and a later `inviteDelivery` (see the
[serve API](../../operate/reference/serve-api.md)).

## Contacts (relay v0.3)

Each of your devices keeps a **contact** on its relays: a random id and a
drop box. Ligus publishes a record there that anyone can check (your account
key, the device's certificate, and its signed encryption key), refreshes it
daily, and announces the contact id on what the device writes, so
co-members can deliver to it.

- An invite to you is sealed to your verified devices before it is dropped.
  The relay can't read it, and garbage that doesn't open is discarded.
- Before dropping an invite, Ligus reads the person's contact record and
  checks it is theirs (signed by their account key, not older than one seen
  before, from a device that wasn't revoked). Otherwise you get the link to
  send yourself.
- New invites show as a banner with the founder's safety code. It says the
  invite is **from** someone only when their device signed it and the code
  matches what you know; otherwise it only **says it's from** them.
  **Accept** joins; **Dismiss** deletes it from the relay.
- A flood of junk into your box from a few addresses evicts its own drops
  first, not invites you haven't read.
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

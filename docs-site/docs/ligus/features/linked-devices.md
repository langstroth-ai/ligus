# Add a computer to your devices

You are one person in a workspace, on every computer you use. If you already
use Ligus on your desktop and want a second computer (the `lt` terminal app,
or the `ligus` command line), **link it as one of your devices**. Don't invite
yourself: an invite always makes a **new member**, and you'd show up as two
people with the same name.

Ligus is short for *ligustica*, the Italian honeybee.

## How it works

1. **On the new computer**, ask to be added:
    - `lt`: on first run choose **Add to my devices** (or, in an install you
      already use, the palette's **Link this computer to my devices**).
    - CLI: `ligus devices request`.

    It shows a **link request** (a code that holds no secret) and a
    **fingerprint**, five groups of four characters.
2. **On your desktop**: **Settings → Profile → My devices → Add a computer**.
   Paste the request. Compare **all five groups** of the fingerprint with the
   computer's screen. Only add a computer you are using right now. Tick
   **The computer shows the same fingerprint** and choose **Add with
   passkey**: one passkey confirmation.
3. The desktop shows a **grant**: copy it to the computer within 30 minutes
   (any way you like: it is sealed so only that computer can open it). The
   desktop also shows its own fingerprint, **This desktop**.
4. **On the computer**, paste the grant (`lt`), or run
   `ligus devices redeem --code-file grant`. It shows who approved it and the
   approving desktop's fingerprint: check it is yours and confirm.

From then on the computer acts as you in that workspace: your name, your
role, your chats.

## What a linked computer can and can't do

It **can** read and post, start chats and workflow runs, answer, run the
agents bound to it, and apply a proposal that someone asked it to apply as a
host.

Some things stay on the desktop that holds your **account key** (the key that
proves who you are, which never leaves that desktop):

- A **vote** from the computer asks for your passkey (`lt` opens it in your
  browser, as for any governance step).
- Adding devices or phones, passkeys and recovery keys, rotating keys,
  linked accounts, creating workspaces and inviting people as their founder.
- Applying a proposal yourself and sharing a file from the computer's disk:
  do those on the desktop.

A linked computer is one person's: it doesn't accept invites, can't be linked
to someone else as well, and isn't a box (a `ligus serve` or `ligus worker`
box joins as its own member, see [Admitting a box](../../operate/ops/serve-box.md)).

## Removing it

**Settings → Profile → My devices → Revoke** on the computer's row (it is
listed as soon as you add it, linked yet or not). Every member stops
accepting what it writes after that. Revoking doesn't take back what it has
already read; to cut that too, an Admin rotates the workspace key (Settings →
Team sync). To link it again, make a new request on it: it uses a new device
key.

## If you already invited yourself

You may already appear twice (for example "Alex" from the desktop and "Alex"
from `lt`). To move to one:

1. In `lt`, open the palette and choose **Link this computer to my devices**.
   Because this install was already in the workspace as its own member, it
   asks with a new device key and proves it was that member.
2. On the desktop, **Add a computer**: the sheet says **This computer was the
   member "Alex (lt)"**. Add it and paste the grant in `lt`.
3. The desktop offers **Remove "Alex (lt)"…**. Removing that member keeps
   every message they wrote under their name (Ligus never changes who wrote
   something) and stops anything new from them. Removing a member takes an
   Owner's or Admin's passkey.

## Troubleshooting

| What you see | Why | What to do |
|---|---|---|
| "this request is too old" | requests last 15 minutes | make a new one on the computer |
| "your desktop's approval hasn't reached this computer yet" | the desktop hasn't synced since you added it | keep the desktop online a moment, then try again (the grant lasts 30 minutes) |
| "already in the workspace as someone else" | this computer's device key is another member's here | make a new request on it (`ligus devices request --new-device`) |
| "the workspace is in lockdown" | dearth mode is on | an Admin turns it off first |
| "add a passkey first" | your account has no passkey here | Settings → Passkeys & keys |
| the relay refuses the computer | a gated relay | sign in as yourself on the computer (`ligus auth login`) |

For how this works underneath (the codes, what is sealed, what each side
checks), see the design note `docs/design/link-a-computer.md` in the
repository.

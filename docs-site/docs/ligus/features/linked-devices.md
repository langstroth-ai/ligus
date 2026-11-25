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
    - CLI: `ligus devices request --copy` (or `ligus devices request | pbcopy`:
      only the request goes to stdout).

    It shows a **link request** (a code that holds no secret) and a
    **fingerprint**, five groups of four characters. Press **c** in `lt` to
    copy the request. Until you finish, `lt` (Settings) and `ligus status`
    say **Linking waiting for your desktop's code** with when it started and
    when it expires; an expired request is cleaned up on its own.

2. **On your desktop**: **Settings → Profile → My devices → Add a computer**.
    Paste the request. Compare **all five groups** of the fingerprint with the
    computer's screen. Only add a computer you are using right now. Tick
    **The computer shows the same fingerprint** and choose **Add with
    passkey**: one passkey confirmation.

    ![Settings, Profile, My devices: this MacBook and a paired phone, with Add a computer (demo data)](../images/desktop-settings-profile-light.png#only-light){ width="720" loading=lazy }
    ![Settings, Profile, My devices: this MacBook and a paired phone, with Add a computer (demo data)](../images/desktop-settings-profile-dark.png#only-dark){ width="720" loading=lazy }

3. The desktop shows the **grant** for the computer, sealed so only that
    computer can open it. Choose **Copy for lt**: it copies the grant alone.
    The desktop also shows its own fingerprint, **This desktop**, large, with
    a copy button.

4. **On the computer**, within 30 minutes:
    - `lt`: paste with ⌘V (Ctrl+Shift+V on Linux) into **Desktop's code** in
      **Add to my devices**. It moves on to the confirmation by itself.
    - A shell: run `lt --plain link-redeem` or `ligus devices redeem` and
      press Enter: it reads the grant from the clipboard (or paste it at the
      hidden prompt). Pasting the whole old command, or a code with line
      breaks around it, works too.

    It shows who approved it, *Approved by Alex's desktop · fingerprint … ·
    for acme. Is that your desktop?*: compare the fingerprint with **This
    desktop** and answer **y**.

    **Copy command** on the desktop copies
    `lt --plain link-redeem --approver-fingerprint <this desktop>`, which
    skips that question: run it, then choose **Copy for lt** and press Enter.
    The command never holds the grant, so the grant stays out of your shell's
    history. In a script: `pbpaste | ligus devices redeem - --approver-fingerprint FP`.

From then on the computer acts as you in that workspace: your name, your
role, your chats.

## Its name, and your earlier member

When it redeems the code, the computer **accepts** your desktop's approval
(it publishes its own copy of the certificate; on a workspace on rules 3,
every workspace 0.4 creates, the approval counts only then), names itself
after its link request (rename it in My devices, or with `ligus devices
rename this NAME` or `lt --plain device-rename NAME` on it), and, if it was
already a separate member here, **retires** that member into you: the member
leaves, and its history stays its own. It does that only when it's safe (the
earlier member ranks no higher than you, isn't the founder or the last
Owner, and has no passkey); otherwise it says so and an Admin removes it in
People.

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

It can't change your name or picture (a stolen one would rename you
everywhere): do that on the desktop. In a workspace it wasn't linked into, it
is read-only and says how to link it there.

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

## Already invited yourself?

If you joined a workspace from `lt` (or the `ligus` CLI) with an invite, that
computer is a **separate member**: People lists it next to you (for example
"Alex" from the desktop and "Alex (lt)" from `lt`), and Workers & hosts lists
its host ("lt on studio-mini"). To make it one of your devices and leave one
"you":

1. **On the desktop**, find it: in **People**, a member that looks like you on
   another computer (the same name, or a shorter name with an `lt` host of its
   own) shows a muted **you on another computer?**. Its row menu (right-click,
   or **⋯**) starts with **Is this you on another computer? Link it to your
   devices…**, which opens **Add a computer** for that member. Ligus only
   suggests it: nothing changes until you add the computer. You can also press
   ⌘K and type **link**, **computer** or **lt** for **Add a computer to my
   devices…**, or ⌘K **My devices**.

    ![People: a member that is you on another computer, with its row menu (demo data)](../images/desktop-people-link-hint-light.png#only-light){ width="560" loading=lazy }
    ![People: a member that is you on another computer, with its row menu (demo data)](../images/desktop-people-link-hint-dark.png#only-dark){ width="560" loading=lazy }

2. **In `lt`**, Settings and People say **This terminal is its own member**.
   Press **D** there (or choose **Link to my devices** in the row menu, `.`,
   or type **link** in the palette, `:`). Because this install was already in
   the workspace as its own member, it asks with a new device key and proves
   it was that member. Press **c** to copy the request.

    <!-- screenshot: lt Settings with "This terminal is its own member · D link to my devices" (lt docs/screens/settings.png) -->

3. **On the desktop**, paste the request in **Add a computer**: the sheet says
   **This computer was the member "Alex (lt)"**. Compare the fingerprint, add
   it with your passkey and paste the grant in `lt`.
4. The desktop offers **Remove "Alex (lt)"…**. Removing that member keeps
   every message they wrote under their name (Ligus never changes who wrote
   something) and stops anything new from them. Removing a member takes an
   Owner's or Admin's passkey. If you opened Add a computer from People and the
   computer didn't prove it was that member, the confirmation says so: remove
   it only if it was you.

## Revoked devices

Revoked devices fold into **Revoked (N)** at the bottom of My devices,
collapsed (Ligus remembers whether you opened it, on this device). Each is one
line, its name and when it was revoked; **Details** in its row menu shows what
each box did (tunnel, push, session). **Hide from this list** hides it here
only: it stays revoked, and every member keeps refusing it. **Show hidden** in
the group's **⋯** brings hidden ones back. A revoked device whose cleanup isn't
finished on some box (a tunnel not cut yet) stays in view above the group with
a one-line warning and **Retry now**.

![My devices: two devices, one revocation still cleaning up, and the Revoked group open (demo data)](../images/desktop-settings-devices-light.png#only-light){ width="720" loading=lazy }
![My devices: two devices, one revocation still cleaning up, and the Revoked group open (demo data)](../images/desktop-settings-devices-dark.png#only-dark){ width="720" loading=lazy }

## Troubleshooting

| What you see | Why | What to do |
|---|---|---|
| "this request is too old" | requests last 15 minutes | make a new one on the computer |
| "the clipboard doesn't hold a desktop code" | something else was copied last | on the desktop choose **Copy for lt** again, then press Enter |
| "the clipboard holds this computer's request" | the computer's own request is still on the clipboard | add it on the desktop first, then **Copy for lt** |
| "your desktop's approval hasn't reached this computer yet" | the desktop hasn't synced since you added it | keep the desktop online a moment, then try again (the grant lasts 30 minutes) |
| "already in the workspace as someone else" | this computer already acted as another member here (it signed as them) | make a new request on it (`ligus devices request --new-device`) |
| "Ben also certified this computer's key. That gives them nothing over it." | another member certified the computer's public key for their own account | nothing: the link goes through, and the computer acts only as you. If you didn't expect it, tell an Admin |
| "the workspace is in lockdown" | dearth mode is on | an Admin turns it off first |
| "add a passkey first" | your account has no passkey here | Settings → Passkeys & keys |
| the relay refuses the computer | a gated relay | sign in as yourself on the computer (`ligus auth login`) |

For how this works underneath (the codes, what is sealed, what each side
checks), see the design note `docs/design/link-a-computer.md` in the
repository.

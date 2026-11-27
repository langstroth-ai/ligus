# Inbox

The **Inbox** is one place for everything that needs **you**. Open it from
**Inbox** at the top of the sidebar, or with the command palette (⌘K, then
"Open Inbox"). Its number is the same one the dock icon and the tray show.
Things that only report what happened (activity, finished work, totals)
are never in it.

![The Inbox: changed sandbox grants, a command to enable, an agent's question with its answers, a vote, a box asking to be added and an invite (demo data)](../images/desktop-inbox-light.png#only-light){ width="760" loading=lazy }
![The Inbox: changed sandbox grants, a command to enable, an agent's question with its answers, a vote, a box asking to be added and an invite (demo data)](../images/desktop-inbox-dark.png#only-dark){ width="760" loading=lazy }

## What's in it

Most urgent first:

| Item | What you do |
|---|---|
| **Another phone answered the code you showed** | **Review** reopens the alert, where you compare fingerprints and keep or revoke the phone you added (see [Ligus on your phone](phone-app.md)) |
| **A box asks to join: “build-1”** · **A computer asks to be your device: “lt on mbp”** | a box, or a computer of yours, asked through the workspace's approval address (0.4). Open it: the sheet shows a code to compare with the device's screen, then **Codes match** and your passkey, or **Don't match**. Nothing is approved from the row itself; more than three collapse into one row ([Approving by code](#approving-by-code), [Admitting a box](../../operate/ops/admitting-a-box.md#code-match), [Add a computer](linked-devices.md#code-match)) |
| **Request from “build-1” didn't match. Nothing was granted.** | the two screens showed different codes, so the request ended and nothing was written. Its **?** says: "The two screens showed different codes, so someone may have tried to intercept the request. Check the device you're adding and start again; tell an Admin if it happens twice." Kept for a day |
| **Your passkey, asked by your device: “laptop”** | another device of yours (the `ligus` CLI) sent a change that needs your passkey; the row says what it does. Open it: the sheet shows the change, worded from the drafted change itself, and the code the other device printed. **Confirm with passkey** signs exactly that change; **Not me** writes nothing ([Passkey hand-off](linked-devices.md#passkey-hand-off)) |
| **You said Not me to a request from “laptop”. Nothing was written.** | **My devices** opens the list where you can revoke that device if you didn't ask for the change; Not me revokes nothing by itself. Kept for a day |
| **Linking “lt on mbp” didn't finish. Nothing was added.** | you matched the codes, but the computer didn't confirm within 2 minutes, so nothing was signed or published and no key left this device. Start again from the computer. Kept for a day |
| **This workspace's sandbox grants changed** | **Review** opens Settings → Sandbox; **Dismiss** marks it seen ([guards](guards.md)) |
| **This workspace wants to run N commands** | **Review** lists each command the repository ships; nothing runs until you enable it ([Trust this workspace](../concepts/security.md#workspace-trust)) |
| **builder asks you** | one row per [question](agent-questions.md) you can answer. Opening it opens the card in its chat, where you answer it with the conversation on screen; nothing is answered from the Inbox. The agent's turn waits until the card is settled. Questions for others show only when nothing else needs you |
| **Your vote: …** | **Review** opens the chat on its Review pane, with the change on screen. You can't approve from the Inbox |
| **Ready to apply: …** | the proposal reached its approvals; **Review** opens it to apply |
| **The last run failed** | a run on this device failed. **Open chat** to see it, or **Dismiss** |
| **serve box says it's your device** | approve it in Settings → Profile with the code it shows (`ligus serve enroll`) |
| **Invites** | a workspace or direct-message invite delivered to this device, with the founder's safety code. **Accept** joins; **Dismiss** deletes it ([invites](invites.md)) |

An approval request (kind `approval`) ranks right after a phone alert, with
the other security items. It leaves the Inbox once it is approved, ended or
expired (a request lasts 15 minutes); the two notices after it are this
device's own record and go on no log.

An item leaves the Inbox when it's done: the question is answered, you
voted, the proposal was applied, you reviewed or dismissed the change. A
proposal you hid for yourself, or one archived for everyone, leaves it too
([Archiving proposals](right-rail.md#archiving-proposals)); an archive never
takes out a proposal that still waits on a decision.

## Approving by code

A box (`ligus admit request --to <address>`) or another computer of yours
(`ligus devices request --to <address>`) sends its request to the
workspace's **approval address**. Copy it from the workspace menu → **Admit
a box** (the first tab), or from Settings → Profile → My devices → **Add a
computer** (and **Add a phone** → Can't scan?). The address isn't a secret
and grants nothing; an Admin can **Reset address**, after which requests to
the old one stop arriving.

The request shows in the Inbox of the people who can approve it: Admins for
a box, and for a device of yours, your desktops that hold your account key.
Clicking it opens a sheet. Its title comes from the kind of request (**Admit
a box?**, **Add a computer to your devices?**); the name the device gave
itself sits beneath it in quotes, never in the title, and Details say when
that name matches a device or member you already have. A phone that sends a
request shows read-only: add phones from My devices → **Add a phone**.

![Admit a box? “build-1”: the code K7Q4 M9X2, large, with Codes match and Don't match (demo data)](../images/desktop-approval-sheet-light.png#only-light){ width="760" loading=lazy }
![Admit a box? “build-1”: the code K7Q4 M9X2, large, with Codes match and Don't match (demo data)](../images/desktop-approval-sheet-dark.png#only-dark){ width="760" loading=lazy }

1. Both screens show the same 8-character code, such as `K7Q4 M9X2`. There
   is no Copy button: the code is for comparing, not for sending. It isn't a
   secret, so it stays visible while you share your screen.
2. Check that the box, or the device you're holding, shows the same code.
3. **Codes match** enables after 3 seconds, then asks for your passkey. The
   passkey sheet names the box or device and the code. For a computer, the
   sheet first says *Waiting for the computer to confirm…*: nothing is
   signed until the computer confirms (2 minutes at most).
4. **Don't match** ends the request. Nothing is granted, and the Inbox keeps
   a note for a day.

The quiet number in the corner is the code's time left (5 minutes). Details
(collapsed) hold the kind, the label, the 80-bit fingerprint, when it was
asked and the relay. When someone else is already checking a request, the
sheet says so ("“Ben's desktop” is checking this request"). If a device that
can't approve opens it after you did, the warning still shows. When a device that
can't approve it opened it, the sheet says it may be an attempt to intercept
it and offers **End this request**.

Pasting a request in **Admit a box** or **Add a computer** still works. When
the pasted text is a request that's also waiting in the mailbox, the same
code sheet opens instead of the fingerprint compare. Otherwise you compare
the fingerprint as before.

## Suggestions

Below the list, quieter and never counted, are suggestions that don't need
you, such as adding the repository of a folder you linked to the workspace
([repositories](repositories.md)).

## What isn't here

- Unread messages and mentions keep their dot in the sidebar
  ([mentions](mentions-and-notifications.md#sidebar-highlights)).
- A chat's live state (Working, Queued, Done) is on its row in the sidebar.
- The Inbox covers the workspace on screen, plus invites and device
  requests, which reach this device whatever workspace is open.

## The status bar and the one banner

State that doesn't need you sits in a quiet strip at the foot of the window,
never at the top:

- **Sync**: Live · room *name* (a lock when it's end-to-end encrypted),
  Offline, Local only, Sync error, a limited sync, or "Set a passphrase to
  sync". Click it for Settings → Team sync.
- **Agents working** and **queued** work (waiting for its host; click to open
  the Review pane, where it's listed).
- A **tip** about growing the workspace, when one applies
  ([guards](guards.md#growing-the-workspace-when-its-ready)).
- **Errors** you haven't looked at: click to open Recent errors.
- **Update available**: click for Download, Later or Skip this version.
- **N need you**, when the sidebar is hidden.

Only a state that blocks work gets a banner across the top, and only one at
a time: a **lockdown** of the open chat, or else **dearth mode**. While both
apply, the lockdown takes the banner and dearth mode shows in the status bar.

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
| **build-1 wants to join** / **lt on mbp wants to be your device** | a box or a device of yours sent its request to the workspace's approval address. Click the row to compare the code ([below](#approving-by-code)); nothing is approved from the row. More than three collapse into one row |
| **Request from build-1 didn't match. Nothing was granted.** | kept for a day. The **?** says what to do: check the device you're adding, start again from it, and tell an Admin if it happens twice. **Linking … didn't finish. Nothing was added.** is the same for a computer that never confirmed. Neither is counted |
| **This workspace's sandbox grants changed** | **Review** opens Settings → Sandbox; **Dismiss** marks it seen ([guards](guards.md)) |
| **This workspace wants to run N commands** | **Review** lists each command the repository ships; nothing runs until you enable it ([Trust this workspace](../concepts/security.md#workspace-trust)) |
| **An agent asked you something** | answer it right there (pick an option or type an answer), or **Open chat**. The agent's turn waits until someone answers |
| **Your vote: …** | **Review** opens the chat on its Review pane, with the change on screen. You can't approve from the Inbox |
| **Ready to apply: …** | the proposal reached its approvals; **Review** opens it to apply |
| **The last run failed** | a run on this device failed. **Open chat** to see it, or **Dismiss** |
| **serve box says it's your device** | approve it in Settings → Profile with the code it shows (`ligus serve enroll`) |
| **Invites** | a workspace or direct-message invite delivered to this device, with the founder's safety code. **Accept** joins; **Dismiss** deletes it ([invites](invites.md)) |

An item leaves the Inbox when it's done: the question is answered, you
voted, the proposal was applied, you reviewed or dismissed the change.

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
Clicking it opens a sheet:

![Admit build-1?: the code K7Q4 M9X2, large, with Codes match and Don't match (demo data)](../images/desktop-approval-sheet-light.png#only-light){ width="760" loading=lazy }
![Admit build-1?: the code K7Q4 M9X2, large, with Codes match and Don't match (demo data)](../images/desktop-approval-sheet-dark.png#only-dark){ width="760" loading=lazy }

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
sheet says so ("Ben's desktop is checking this request"). When a device that
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

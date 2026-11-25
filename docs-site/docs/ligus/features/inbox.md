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
| **This workspace's sandbox grants changed** | **Review** opens Settings → Sandbox; **Dismiss** marks it seen ([guards](guards.md)) |
| **This workspace wants to run N commands** | **Review** lists each command the repository ships; nothing runs until you enable it ([Trust this workspace](../concepts/security.md#workspace-trust)) |
| **An agent asked you something** | answer it right there (pick an option or type an answer), or **Open chat**. The agent's turn waits until someone answers |
| **Your vote: …** | **Review** opens the chat on its Review pane, with the change on screen. You can't approve from the Inbox |
| **Ready to apply: …** | the proposal reached its approvals; **Review** opens it to apply |
| **The last run failed** | a run on this device failed. **Open chat** to see it, or **Dismiss** |
| **serve box says it's your device** | approve it in Settings → Profile with the code it shows (`ligus serve enroll`) |
| **Invites** | a workspace or direct-message invite delivered to this device, with the founder's safety code. **Accept** joins; **Dismiss** deletes it ([invites](invites.md)) |

An item leaves the Inbox when it's done: the question is answered, you
voted, the proposal was applied, you reviewed or dismissed the change. A
proposal you hid for yourself, or one archived for everyone, leaves it too
([Archiving proposals](right-rail.md#archiving-proposals)); an archive never
takes out a proposal that still waits on a decision.

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

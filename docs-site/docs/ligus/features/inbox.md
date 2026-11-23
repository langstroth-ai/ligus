# Inbox

The **Inbox** is one place for everything that needs **you**. Open it from
**Inbox** at the top of the sidebar, or with the command palette (⌘K, then
"Open Inbox"). Its number is the same one the dock icon and the tray show.
Things that only report what happened (activity, finished work, totals)
are never in it.

## What's in it

Most urgent first:

| Item | What you do |
|---|---|
| **Another phone answered the code you showed** | **Review** reopens the alert, where you compare fingerprints and keep or revoke the phone you added (see [Ligus on your phone](phone-app.md)) |
| **An agent asked you something** | answer it right there (pick an option or type an answer), or **Open chat**. The agent's turn waits until someone answers |
| **Your vote: …** | **Review** opens the chat on its Review pane, with the change on screen. You can't approve from the Inbox |
| **Ready to apply: …** | the proposal reached its approvals; **Review** opens it to apply |
| **The last run failed** | a run on this device failed. **Open chat** to see it, or **Dismiss** |
| **This workspace's sandbox grants changed** | **Review** opens Settings → Sandbox; **Dismiss** marks it seen ([guards](guards.md)) |
| **This workspace wants to run N commands** | **Review** lists each command the repository ships; nothing runs until you enable it ([Trust this workspace](../concepts/security.md#workspace-trust)) |
| **serve box says it's your device** | approve it in Settings → Profile with the code it shows (`ligus serve enroll`) |
| **Invites** | a workspace or direct-message invite delivered to this device, with the founder's safety code. **Accept** joins; **Dismiss** deletes it ([invites](invites.md)) |

An item leaves the Inbox when it's done: the question is answered, you
voted, the proposal was applied, you reviewed or dismissed the change.

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

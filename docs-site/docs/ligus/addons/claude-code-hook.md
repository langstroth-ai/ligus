# Claude Code's permission prompts

A Claude Code session that Ligus didn't start (one you run yourself in a
terminal) asks before it runs a command or edits a file. With
`ligus hook permission` installed as its `PermissionRequest` hook, that
prompt also reaches your Ligus devices as a **grant card**. You can answer
it from the desktop or your phone while the terminal waits.

## What it does

- Claude Code runs the hook when it is about to show a permission dialog
  (and in `-p` mode, where nobody could answer one). The hook reads the
  prompt on stdin.
- The hook asks the **running Ligus desktop app** over a socket in the
  app's data dir. The socket is owner-only (0600), and the hook presents the
  install's hook token (`ligus hook token` says where it is; the token itself
  is never printed). The hook never opens a data dir itself.
- The desktop posts a grant card in the chat you named. The card is signed
  by your device as you, is put to **you alone**, and its header says
  "Claude Code (outside Ligus)". It shows the call's own content: the exact
  command, or the file and its full new content (or the old and new text of
  an edit). A call over 8 KiB offers only **Deny**, and the card says to
  answer at the terminal. What Claude Code suggests (`permission_suggestions`,
  the MCP server) is listed under Details and is never applied.
- **Allow once always needs your passkey** on the desktop and Face ID on
  the phone. Ligus didn't start this agent and doesn't know its sandbox.
- The hook then prints Claude Code's decision:
  - **Allow once**: `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`.
    Never `updatedInput` or `updatedPermissions`: Ligus adds no standing
    rule.
  - **Deny**: `behavior: deny` with the message "Denied from Ligus by Ana"
    and `interrupt: false`.
  - **No answer** within `--timeout` (90 s by default), **Not now**, or
    **no Ligus running**: nothing. Claude Code then shows its own dialog, or
    denies the call in `-p` mode. The card is closed.
- It always exits 0. Past 10 prompts a minute, or with 3 already waiting,
  it prints nothing.

A `ligus serve` box answers the hook with no decision: a box is its own
member, so a card it posted could never be your grant. Run the desktop app
for hook prompts. (`lt` doesn't take hook prompts yet.)

## Where the cards go: a workspace of your own

Every member of a workspace can read every chat in it, and a permission
prompt can show commands, paths and file content. Send hook prompts to a
workspace of **one**, the "Personal" workspace the desktop offers to make,
never to a shared chat. The hook warns on stderr, once per chat, when other
members can read the chat.

## Set it up

1. In the desktop app, make (or open) your Personal workspace and a chat for
   prompts. Copy the workspace id and the chat id (`ligus chats --json`
   lists chats with their workspace).
2. Install the hook into the settings file of a project, or into your own
   settings:

    ```sh
    ligus hook install --settings .claude/settings.json \
      --workspace <workspace-id> --chat <chat-id> --dry-run   # shows the change
    ligus hook install --settings .claude/settings.json \
      --workspace <workspace-id> --chat <chat-id>
    ```

    It keeps everything else in the file, and running it again changes
    nothing. It writes:

    ```json
    {"hooks": {"PermissionRequest": [{"matcher": "*", "hooks": [
      {"type": "command", "command": "ligus hook permission --workspace W --chat C", "timeout": 120}]}]}}
    ```

    Installing into your own `~/.claude/settings.json` reaches every Claude
    Code session you run, so it asks for your yes at the terminal (and exits
    4 without one). Nobody installs it for you.

3. Keep the hook's wait (`--timeout`, default 90 s, at most 590 s) under the
   hook's `timeout` in the settings. You are blocked at the terminal while it
   waits, which is why the default is short.

## What Claude Code sends (checked)

Checked against Claude Code 2.1.332 and the hooks reference
(code.claude.com/docs/en/hooks), with a recording hook in a throwaway
project:

- The hook fires on `PermissionRequest` in interactive sessions and in
  `-p`. The input has `session_id`, `transcript_path`, `cwd`,
  `permission_mode`, `hook_event_name`, `tool_name`, `tool_input`,
  `permission_suggestions` and `prompt_id`, plus `mcp_server` for MCP tools.
- **Sub-agents' prompts fire it too**, with `agent_id` and `agent_type`
  (for example `general-purpose`). The card then says "through its
  general-purpose sub-agent".
- In `-p`, a hook that prints nothing leaves the call denied (it shows in
  `permission_denials`). An allow from the hook runs the call, and a deny
  stops it.
- Exit code 2 is not a decision on this event; the hook never uses it.

## Limits and risks

- Anything running as you can read the hook token, so it can raise prompts
  to you. It can't grant them: a grant needs your passkey, and
  `ligus asks answer` refuses hook prompts outright.
- An agent running as you outside Ligus can still read that install's
  device key and answer *questions* as you, never grants.
- See also [Questions to the chat](ligus-mcp.md#asks) and
  [`ligus asks`](../reference/cli.md#asks).

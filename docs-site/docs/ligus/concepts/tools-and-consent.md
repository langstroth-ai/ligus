# Tools & permissions

Agents in Ligus reach the outside world through **MCP tools**, and the
authority they have over your files depends on the **runtime** they run
on. There is no separate Ligus-owned tool registry or inline consent
banner — file and command access is governed by the two mechanisms
below, plus the human review gate on proposals.

## MCP tools

Model Context Protocol servers are the way you give agents tools
(filesystem, search, a ticketing system, anything with an MCP server).
Add them in the **Tools** pane of the right rail (or in
`ligus.config.toml` under `[[mcp_servers]]`).

Two properties make this safe by default:

- **Inert until enabled.** A newly added MCP server is disabled; its
  tools are never offered to a model until you flip it on. Enabling is
  an explicit, per-server action.
- **Only some runtimes drive them through Ligus.** Ligus runs the MCP tool
  loop for runtimes on the Anthropic API, and for Ollama and local
  OpenAI-compatible (`custom`) runtimes with **Supports tools** on (those
  get at most 16 tools a turn; see
  [MCP servers](../addons/mcp.md#local-models-at-most-16-tools-a-turn)).
  Subprocess runtimes (Claude Code, Codex, aider, pi, Google Antigravity,
  GitHub Copilot) bring their own tools and their own permissioning — see
  below.

Enabled servers show their connection status in the Tools pane.

## File & command authority (Claude Code and the subscription CLIs)

When a chat runs on the **Claude Code** runtime, the agent edits files
and runs commands through Claude Code itself, and you choose how much
it may do under **Settings → Permissions** (the `claude_permission_mode`
setting). Claude Code runs headless, so it can't prompt: whatever the mode
doesn't allow is blocked.

- **default** — read-only; edits and commands are blocked.
- **acceptEdits** — file edits are allowed; commands are blocked.
- **runCommands** — edits plus shell commands (build, test, git). The shell is only used in turns the workspace Owner starts, or by an agent an Owner granted it ([the shell is the Owner's](security.md#owner-only-shell)); other members' turns run at most at accept edits.
- **bypassPermissions** — everything, without asking (use only in a
  sandbox you trust).

The **Google Antigravity** (`agy`) and **GitHub Copilot** (`copilot`)
runtimes follow the same setting, mapped to each CLI's own permission
flags; see
[Bring-your-own agents](../addons/byo-agents.md#subscription-clis-google-antigravity-and-github-copilot).

This is each CLI's own permission model, surfaced in Ligus's settings —
Ligus does not intercept individual tool calls itself.

## The human review gate (proposals)

The strongest control is agreement-gated execution. An agent can raise
a **proposal** — a titled change or decision — that lands in the
**Review** pane. A proposal does nothing on its own: a human (or a
quorum of them, when `required_approvals > 1`) approves it, and only
then does clicking **Implement** dispatch it to an agent to carry out.
[Workflows](../features/workflows.md) use exactly this mechanism for
their approval gates, and for the workflows themselves: an agent's workflow
is a proposal, and approving it adds the definition (which still runs only
when a person starts it).

## Threat model

What these controls protect against:

- ✅ An agent using a tool you never enabled — MCP servers are inert
  until you turn them on.
- ✅ Unreviewed changes landing silently — proposals require explicit
  human approval before they are implemented.

What they do **not** protect against:

- ❌ The runtime endpoint itself. If you point a runtime at a malicious
  endpoint, that endpoint sees your conversation. Use providers you
  trust.
- ❌ MCP servers you enable. An MCP server runs on your machine with
  whatever permissions its binary has. Vet the binary before enabling.
- ❌ A subprocess agent's own tools. Under `bypassPermissions`, Claude
  Code acts without prompting — that's the setting's purpose; scope it
  to a sandbox.

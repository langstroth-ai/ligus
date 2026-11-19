# Right Rail

The right side of the window is a single utility pane with a
vertical icon strip you click to switch contents. Nine sections:

| Pane      | What's in it                                                    |
|-----------|-----------------------------------------------------------------|
| Tools     | Runtimes, workspace agent CRUD, MCP servers                     |
| Context   | Context-budget telemetry + summarize/compact controls           |
| Review    | Pending proposals (quorum votes + agreement-gated implement) **and** queued work: unanswered agent mentions + host status |
| People    | Members, presence, roles, invites                               |
| Vaults    | Reference-material sources mounted into the session             |
| Skills    | Instruction bundles injected into participants' prompts         |
| Workflows | Agent-written pipelines: read-only definitions (run, pause, delete, ask an agent to change), live runs, gates. See [Agentic workflows](workflows.md) |
| Flow      | The task board, review queue and hand-off waits, workspace marks, routing, careers, path zones and the home beacon. See [Flow](flow.md) |
| Memory    | The workspace memory: shared notes agents read and propose changes to, and the proposals waiting for a person. See [Workspace memory and task state](memory.md) |
| Activity  | Streaming runtime activity for the workspace                    |

<!-- Screenshot pending (Right rail with the Tools pane active): images/right-rail.png. See images/CAPTURE_GUIDE.md; restore the image line once it is captured. -->
Switching panes preserves the visible width. Click the `×` in the
pane header to close the rail entirely.

## When to use each

- **Tools**: workspace agents (add/edit/remove), the active runtime,
  and MCP servers — open it when a run is in flight (it shows each
  agent's planning → working → awaiting-review state) or when an MCP
  server is misbehaving and you want to disable a tool. The wrench
  glyph in the chat header also opens this pane.
- **Context**: when a chat's reasoning depends on specific facts.
  Shows the context-budget telemetry and the summarize/compact
  controls; pinning a context item bumps it to the top of the
  conversation prompt.
- **Review**: when an agent has proposed a write. An agent raises a
  proposal by ending a reply with a `[[propose: …]]` directive; it's saved
  here for review, never auto-run. Lists pending proposals, lets you claim /
  approve / disposition — approval counts at the voter's real role and an
  author can't approve their own — then **Implement** once quorum is met.
  See [agent-authored proposals](multi-agent.md#agent-authored-proposals).
  Each card shows what the proposal is and where it came from, as badges:
  its **kind** (file change, command, decision, workflow), its **author**
  (a person, or an agent with "agent-signed" when its own key signed it, or
  "host-signed (legacy)" when its host signed for it; an author who isn't a
  member, such as the chat's primary runtime, always shows as an agent),
  **Config impact** when it touches configuration, CI, hooks, protected dirs
  or MCP config (paths match ignoring case, and a command that runs inline
  interpreter code such as `sh -c` is always flagged),
  **Human gate**, **Presence required**, and **No exact command** for a
  command filed without an argv. **Implement…** first shows exactly what
  will run: the argv of a command (run as-is in the chat's worktree,
  sandboxed and offline, with no model involved), or the instruction text
  an agent receives, and which agent (by default the one that produced the
  proposal), on which runtime and driver, in which sandbox, with which
  network. The preview and the run come from the same function.
  **Apply on ‹host›** asks one host (a `ligus serve` box, a worker, or a
  desktop) to apply an approved proposal instead; that host applies it
  once, in the chat's worktree of its checkout, and the card shows the
  request as *open*, *applied* (with the commit), *failed* (with the
  host's reason) or *withdrawn*. While a request is open, nobody else
  applies the proposal (Implement says so) and no second request is taken;
  its requester or an Admin can **Withdraw** it. Approving never applies
  anything on its own. See
  [Apply on a host](../../operate/ops/serve-box.md#apply-on-a-host).
  **Questions from agents** (`ligus_ask_human`, and ACP permission requests
  that need a person) appear at the top of the pane with their options; the
  asking turn waits for your answer. See
  [Ligus tools for agents](../addons/ligus-mcp.md#asks).
  **Proposal sets** (competing alternatives with scout scores, the quorum
  dial, and pre-flight then apply) sit at the top of the pane; see
  [Exploring options](proposal-sets.md).
  It also shows
  **Queued work** — every unanswered `@agent` mention with its host's
  live status (online / offline / owner-device). If an agent's host is
  offline, a **Run on worker** button reassigns it to an online worker
  so the backlog drains without waiting for that laptop to wake. See
  [Headless agents](../../operate/concepts/headless-agents.md).
- **People**: members, online status, invites, cross-network
  rendezvous status, nearby-on-LAN peers. A member's role picker,
  **Remove**, **Remove & revoke** and **Allow new passkey** are disabled
  where the workspace would refuse them, with the reason on hover: an
  Admin can't remove or demote another Admin, only the founder cuts an
  Owner, nobody cuts the founder, a service member (a box) is never raised
  above Contributor and holds no passkey. The pane asks the same rule
  function every device's fold uses. On a membership-enforcing relay it also
  manages the server-side roster + issues join codes, and — for an admin with a
  relay issuer key (Settings → Team sync) — a **Remote agents** section that
  enrolls a headless agent and generates the one copy-paste command to run on its
  box. See [Headless agents](../../operate/concepts/headless-agents.md).

- **Flow**: when work is piling up or nobody picked something up. The
  Board lists unassigned work and who would take it; Queue shows the
  review gate and where work waits; Marks, Routing, Roster, Zones and Home
  cover the rest. See [Flow](flow.md).

## Why a single rail

The icon strip + tinted card is the only chrome that needs to
exist for these signals. A window-level toolbar duplicating the
same affordances would just steal screen space; the rail is the
canonical place to switch panes and close them.

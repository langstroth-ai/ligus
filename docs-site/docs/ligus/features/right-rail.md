# Right Rail

The right side of the window is a single utility pane with a
vertical icon strip you click to switch contents. Ten panes:

| Pane      | What's in it                                                    |
|-----------|-----------------------------------------------------------------|
| Agents & tools | Runtimes, workspace agent CRUD, MCP servers                |
| Context   | Context-budget telemetry + summarize/compact controls           |
| Review    | Pending proposals (quorum votes + agreement-gated implement) **and** queued work: unanswered agent mentions + host status |
| People    | Members, presence, roles, invites                               |
| Vaults    | Reference-material sources mounted into the session             |
| Skills    | Instruction bundles injected into participants' prompts         |
| Workflows | Agent-written pipelines: read-only definitions (run, pause, delete, ask an agent to change), live runs, gates. See [Agentic workflows](workflows.md) |
| Flow      | The task board, review queue and hand-off waits, workspace marks, routing, careers, path zones and the home beacon. See [Flow](flow.md) |
| Memory    | The workspace memory: shared notes agents read and propose changes to, and the proposals waiting for a person. See [Workspace memory and task state](memory.md) |
| Activity  | Streaming runtime activity for the workspace                    |

<div class="grid" markdown>

![The Agents & tools pane: the runtime, three workspace agents and two MCP servers (demo data)](../images/desktop-tools-light.png#only-light){ width="340" loading=lazy }
![The Agents & tools pane: the runtime, three workspace agents and two MCP servers (demo data)](../images/desktop-tools-dark.png#only-dark){ width="340" loading=lazy }

![The Review pane: a proposal with its diff, a comment, one of two approvals and Approve or Reject (demo data)](../images/desktop-review-light.png#only-light){ width="340" loading=lazy }
![The Review pane: a proposal with its diff, a comment, one of two approvals and Approve or Reject (demo data)](../images/desktop-review-dark.png#only-dark){ width="340" loading=lazy }

</div>

Switching panes preserves the visible width. Click the `×` in the
pane header to close the rail entirely. Each pane's header has a small `?`
beside its title: hover it for what the pane is for, click it to open the
pane's page in these docs.

## When to use each

- **Agents & tools**: workspace agents (add/edit/remove), the active runtime,
  and MCP servers — open it when a run is in flight (it shows each
  agent's planning → working → awaiting-review state) or when an MCP
  server is misbehaving and you want to disable a tool. The wrench
  glyph in the chat header also opens this pane. An agent's **⋯** menu
  has **Change model…** and **Runs on…** (see
  [Changing an agent's model or where it runs](workspace-rules.md#change)).
  An agent whose runtime keeps a model server's address on the workspace
  shows one quiet line under it, with **Move it to this computer** on the
  computer that runs it (see
  [Model server addresses](workspace-rules.md#model-server-addresses)).
  To remove an agent, open
  its **⋯** menu and pick **Remove from workspace…**; the confirmation says
  what follows: the agent leaves every chat in the workspace for everyone,
  mentions of it go unanswered, workflow stages that use it stop with an
  error, and its avatar and access settings are deleted.
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
  A proposal's description renders as Markdown. Once you've voted, the
  card says **You approved** (or **You rejected**) with a link to change
  your vote. **Implement** shows only when it can run: a proposal rejected
  at or above the approval floor, or replaced by a newer revision, says so
  instead, even when it has its quorum of approvals.
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
  its requester or an Admin can **Withdraw** it. Approving applies
  nothing on its own, with one exception: when your vote on the desktop is
  the one that approves a workflow proposal, it adds the workflow to the
  **Workflows** pane then and there (it doesn't run it). See
  [Apply on a host](../../operate/ops/serve-box.md#apply-on-a-host).
  **Questions from agents** (decision cards, and ACP permission requests
  that need a person) appear at the top of the pane while they are open;
  the asking turn waits for your answer. See
  [Questions from agents](agent-questions.md).
  **Proposal sets** (competing alternatives with scout scores, the quorum
  dial, and pre-flight then apply) sit at the top of the pane; see
  [Exploring options](proposal-sets.md).
  Settled proposals (applied, rejected, or replaced by a newer revision)
  fold into **Settled**, and old ones leave the pane; see
  [Archiving proposals](#archiving-proposals) below.
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
  function every device's fold uses. Under Members, one line says whether
  the relay checks membership for this workspace, with the details behind
  **?** and at most one action: **Claim ownership** only when this device's
  relay token names an account, **Add a relay token** on a self-hosted relay
  when it doesn't, and nothing on the community relay or a relay with
  membership off. See
  [Claiming a workspace](../../operate/ops/server-side-membership.md#claiming-a-workspace).
  On a membership-enforcing relay it also
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

## Archiving proposals

Old proposals stop taking up room in the Review pane. Nothing is deleted:
every proposal stays in the chat's log and can be brought back.

- **Archive** (a settled proposal's row menu) takes it out of the inbox for
  everyone in the workspace. **Archive settled** does it for the whole
  Settled shelf. A Contributor or above archives; the change syncs like a
  vote. A proposal that still waits on someone (open, or approved and not
  yet applied) can't be archived: reject it first, or hide it for yourself.
  An archive from someone below Contributor is ignored by the inbox.
- **Hide for me** (any proposal's row menu) takes it out of your inbox on
  this device only. Nobody else is affected, and it isn't synced to your
  other devices: Ligus has no per-person settings channel between your own
  devices, so each device keeps its own hidden list.
- **Auto-archive**: a settled proposal leaves the inbox on its own once it
  has been settled for the workspace's auto-archive days (7 by default;
  an Admin changes it, or turns it off, in **Settings → Review**). This is a
  rule every device applies to the time the proposal settled (the time
  recorded on the event that settled it, never a device's own clock), so no
  device writes anything when a proposal ages out and every device agrees.
  A settled proposal restored after it was auto-archived stays in the inbox.

Archived and hidden proposals drop out of the Inbox, the needs-you counts,
the dock badge and what agents read with `ligus_read` (an agent can ask for
them with `archived: true`). They are listed in **Archived proposals**:
the Review pane's **⋯** menu, or ⌘K **Archived proposals**. It covers every
chat of the workspace and has a search box (title, chat, description, files).
Click a row to open it in its chat; its row menu has **Restore** (back in the
inbox for everyone) or, for one you hid, **Show again** (back on this device).

In lt the same actions are in a proposal's row menu, with an archived filter.
On the phone, a settled proposal's menu has **Archive**, and archived ones
are listed apart; see [the phone app](phone-app.md).

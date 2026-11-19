# The agent-turn engine in hive-runtime

Status: implemented on `feat/turn-engine` (Ligus 0.1.0). Ligus is short for
ligustica.

## Why

An agent turn (stream a reply, offer tools, isolate edits in a per-turn
worktree, file the diff as a proposal, run the Ligus tools, finalize the
reply's directives) was implemented twice: in the desktop
(`app/src/lib.rs`: `run_prepared_turn_inner`, `try_tool_loop`,
`mcp_tool_set`, `agent_iface.rs`) and in the headless worker
(`crates/hive-cli/src/worker.rs` `run_turn`, `reply.rs`). The terminal client
`lt` could only stream plain chat, because neither copy is reachable from a
crate it depends on. The engine is that code once, in
`hive_runtime::turn_engine`, so the desktop, `ligus worker` / `ligus serve`
and `lt` run the same turn.

## Boundaries

The engine **runs a turn that has already been claimed**. It does not decide
whether a turn runs here, and it does not claim it: that stays with the
dispatch gates (`turn_dispatch`, `claim_turn`, the device dispatcher).

It also does not build the turn's context (the desktop's windowed context,
summaries, retrieval and flow priming; the worker's `turns_for`), and it does
not decide the turn's policy. The host decides the author gate, the shell
rule and the sandbox envelope, and hands the results in. Those rules change
on their own branches (the Phase C sandbox), and they apply unchanged when
they are inputs.

```
host (desktop | worker | lt)                       hive_runtime::turn_engine
-------------------------------------------         ------------------------------------
claim (turn_dispatch / dispatcher)
context: system prompt + turns
gate: author role -> GatePlan, shell rule,
      envelope + enforcement, grants
TurnPolicy (base), executor
                                     ---- run(host, TurnInput) ---->
                                                    begin_assistant_message
                                                    host.checkout(...)   (repo gate)
                                                    worktree: eager / lazy / in place
                                                    Ligus MCP turn (folded or socket)
                                                    tools: built-in + MCP + Ligus
                                                    stream (Anthropic loop | dispatch)
                                                       under executor + TurnPolicy
                                                    finalize directives + effects
                                                    diff -> FileDiff proposal
                                                    self-medication, release worktree
                                     <---- TurnOutcome / TurnError --
UI events (Tauri, phone sink, lt Event)  <- TurnHost sink callbacks during the run
```

## API

```rust
pub async fn run<H: TurnHost>(host: &H, input: TurnInput) -> Result<TurnOutcome, TurnError>;
```

`TurnInput` (everything the host decided):

- ids: `workspace_id`, `chat_id`, `reply_to` (the trigger);
- `responder`: author (message author), effect author, runtime id, agent id,
  and the runtime **as it will run** (gate tier, shell-free args and envelope
  flags already applied by the host);
- `system`, `turns`: the context the host built;
- `gate: GatePlan`, `envelope: SandboxEnvelope`, `shell_allowed: bool`;
- `policy: TurnPolicy`: the base sandbox policy (gate, envelope, shell,
  grants, MCP ids, presets). The engine only sets what it alone knows: the
  worktree's isolation and the Ligus MCP socket;
- `executor: Arc<dyn TurnExecutor>`: every child the turn spawns is confined
  by it (`executor::with_executor` scopes it next to the policy);
- `tools: ToolOptions`: built-in tools on, write in place, `run_command`
  (already narrowed by the envelope and the shell rule), MCP after reads,
  remote file tools;
- `write_capable`, `require_isolation`: whether the CLI's tier edits files,
  and whether such a turn may ever run in place;
- `stop: Option<Arc<Notify>>` (cancellation) and the turn timeout;
- small per-host switches that keep each host's behaviour as it was
  (git attribution for CLI agents, the empty-reply notice, the error prefix,
  injection scanning, the self-report's confidence and turn notes).

`TurnHost` (what differs per host), all but `service` with defaults:

- `service()`: the `Mutex<ChatService>`, locked only for short store calls,
  and `pin()`, called on every lock (a client with several workspaces pins
  the service to the turn's founder);
- catalogs: `mcp_tools(egress, shell)` builds the turn's MCP tool set (the
  desktop's managed and workspace catalogs with trust and OAuth, the CLI's
  `LIGUS_MCP_CONFIG`, none for lt);
- `ligus()`: this host's Ligus MCP server, and `directive_guidance()`;
- checkout provider: `checkout(session, message_id)` runs the repo gate and
  names the checkout, the chat worktree, the base and the prompt context;
  `cut_worktree`, `release_worktree`, `reuse_worktrees`;
- `workflow_tool(session)`: in-turn workflow validation (desktop);
- finalize and proposals: `finalize` (the shared one by default),
  `before_file_proposal`, `proposal_filed`;
- the event sink: `begun` (the placeholder is posted), `delta`, `activity`
  (tool calls and results, thinking), `phase` (completed / error),
  `changed`, `locked_down`;
- telemetry hooks (content-free): tools selected, progress, egress denials,
  usage errors, plan windows.

`TurnOutcome` is the committed answer: the message id, the cleaned body, the
proposal filed from the worktree (if any) and whether the reply was empty.
`TurnError` carries the message the host shows, and whether the person
stopped the turn.

Presence: a turn does not read presence. The dispatch gates (which consult
presence and host bindings) run before the engine; a hand-off the Ligus
tools start goes back through the host's `LigusHost::start_turn`, which runs
its own gate.

Shared helpers for a host without rules of its own (`lt`):
`decide_gate` / `decide_gate_with` (the author gate, the shell rule and
grants, the envelope, the OS sandbox, as the desktop and the worker decide
them; `Gated::policy` builds the base `TurnPolicy`) and `repo_checkout` (the
repos v2 gate with a legacy bound checkout).

## What moves, what stays

Moves to `hive_runtime::turn_engine`:

- the Anthropic tool loop adapter and the MCP tool executor (both hosts had a
  copy), tool-name sanitizing, turns to messages;
- the turn runner: placeholder, checkpointed deltas, worktree choice (eager
  for writing CLI agents, lazy on first write for HTTP tools, in place
  otherwise), built-in tools rooted at the chat's base worktree, the
  composite tool set with hints, Ligus tools folded or over the socket,
  stream under the executor and policy, stop and timeout, finalize, the diff
  as a FileDiff proposal, self-medication, worktree release, the Ligus
  self-report;
- the Ligus per-turn interface (`agent_iface::TurnIface` and the worker's
  copy);
- the injection-marker collector.

Stays in the hosts:

- desktop: Tauri events and window state (the sink emits
  `ChatStreamEvent` / `ChatActivityEvent`), settings, MCP catalogs and trust,
  flow priming and after-turn facts, proposal-set grouping, guard telemetry,
  the windowed context and summaries, revision turns and `implement_proposal`
  (they call the same runner with their own context);
- worker / serve: mention draining, claiming, runtime resolution, the box's
  repo gate with clone waiting, the phone's turn sink;
- lt: its dispatch gates and claim, and its UI (a tool-activity view fed by
  `Event::ToolActivity`).

## Tests

Engine-level, in `hive-runtime` (stub model servers, temp repos, in-memory
stores): the tool loop with a stub OpenAI-wire model, built-in file tools,
an MCP tool call through a stub HTTP MCP server, the per-turn worktree and
the proposal it files, a Ligus hand-off, cancellation through the stop
signal, and policy enforcement (no `run_command` without the shell rule,
and a confined spawn goes through the given executor, whose plan denies the
home dir). The desktop's, the CLI's and the e2e tests run unchanged.

`lt`: an integration test where a stub model calls a tool, edits a file in
the turn's worktree and files a proposal, synced through the test relay with
temp data dirs.

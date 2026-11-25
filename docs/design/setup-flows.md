# Guided setup flows (design note)

Status: design, owner-approved scope (2026-11-24). Targets: the `ligus` CLI
(`ligus setup …`), the desktop (⌘K palette, composer `/setup`, pane empty
states) and, next, `lt` (ligus-term), all on one engine in `ligus-runtime`.
Ligus is short for *ligustica*, the Italian honeybee.

## 1. Goal

Seven guided flows, the same on every surface:

| Flow (`ligus setup <id>`) | Palette label | Ends with |
|---|---|---|
| `agent` | Set up an agent | an agent on a runtime whose CLI is installed and signed in |
| `tool` | Connect a tool | an MCP server that answers its handshake, reachable by the agents chosen |
| `folder` | Add a code folder | a workspace repo bound to a folder on this device |
| `box` | Add a box | a box admitted by a passkey, `ligus serve` running on it |
| `phone` | Add a phone | the phone paired through the existing pairing screen |
| `invite` | Invite a person | an invite created as today, acceptance tracked |
| `relay` | Set up a relay | a relay that passes `check`, this workspace switched to it |

Each flow is **preflight checks → steps → verify → done**. Every step is
idempotent (re-running a finished flow changes nothing and says so),
cancellable between steps, and never leaves half a configuration: all input
is validated before the first write, and a flow with several writes undoes
the ones it made if a later one fails.

`ligus setup` with no flow lists the seven flows and what is already done.

## 2. What exists (code-verified)

- `ligus setup [--apply]` already exists (`crates/ligus-cli/src/main.rs`
  `cmd_setup`): it detects API keys, Ollama, `claude`, `codex`, `agy`,
  `copilot` and defines the keyless ones as workspace runtimes. It becomes
  `ligus setup runtimes [--apply]`; `ligus setup --apply` keeps working for
  one release with a note on stderr.
- Check reports: `ligus.checks.v1` (`crates/ligus-cli/src/checks.rs`), exit
  codes 0–7 (`problem.rs`). The `Check`/`Status`/`Report` types move to
  `ligus_runtime::checks` so the engine emits them; the CLI re-exports them
  unchanged (same JSON, same text form).
- Detection: `cli_detect::detect_cli_agents` (agy, copilot, gemini),
  `executor::cli_home::expects_file_sign_in` (Claude Code, Codex sign-in
  without reading a token), the Ollama probe and `on_path` (duplicated in the
  CLI and the desktop). Model lists: `model_catalog::cli_models`; Claude
  Code's list is desktop-only (`app/src/chat_models.rs`). Sandbox:
  `executor::sandbox_status()`.
- Agents: `ChatService::add_agent` (Contributor). Name rules live only in the
  desktop's `add_agent_sync`: not empty, not `is_reserved_agent_name`
  (#237's list: primary, ligus, hive, you, all, everyone, owners, admins,
  contributors, viewers, and look-alikes), no clash with an agent or member
  name. The CLI checks only the reserved list. The engine owns all three.
- Runtimes: the desktop keeps device-local runtimes in `ligus.config.toml`
  (`add_runtime`); the CLI defines workspace runtimes
  (`WorkspaceRuntimeUpserted`, **Admin**, they can carry credentials).
- MCP: `McpServerSpec` (`mcp.rs`), handshake `McpRegistry::list_tools`. The
  desktop stores servers in `<workspace>/.ligus/mcp-servers.json` and tokens
  in the secret store (`mcp_bearer_tokens/*`, scope `settings`); the CLI reads
  a TOML file named by `LIGUS_MCP_CONFIG` whose `token_env` names an env var.
  **No per-agent MCP access exists**: every agent's turn sees every enabled
  server its egress policy allows.
- Folders: `repos::bind` (git checks: a checkout, not a linked worktree, the
  remote matches, not managed elsewhere, folder not taken) and
  `ChatService::add_workspace_repo` (Admin). Default-branch detection is
  copied three times (`app/src/repos_ui.rs`, `ligus-cli/src/repos.rs`,
  `repos_cmd.rs`); the engine gets the one shared copy.
- Box: `ligus admit request` / `redeem` on the box (`admit.rs`); an Admin
  approves with a passkey in the desktop's **Admit a box** (`AdmitBoxModal`).
- Phone: `ligus pair` prints a pairing secret; the desktop's
  `AddPhonePanel` runs the QR pairing with `phone_enroll_precheck`.
- Invites: desktop `workspace_invite_sync`; `ligus serve`'s `POST
  /v1/invites`. No `ligus invite` command. Issuing is `JoinApproved`
  (**Admin**, presence on rules v2). Acceptance is visible as the invite id in
  `guards.used_invites`.
- Relay: `RelayClient::probe`; `usable_relay_url` accepts http and https. The
  https rule exists only for the phone tunnel.

## 3. The engine (`ligus_runtime::setup`)

A pure orchestration layer. It owns flow definitions, fields, validation,
check ids and fix lines, the step order, idempotency, role gates and the
secret rules. It does no I/O itself: everything that touches the world goes
through a `SetupOps` trait that each surface implements (the CLI over its
`Config` and `ChatService`, the desktop over `AppState`, lt over its own).

```rust
pub fn flows() -> &'static [FlowDef];                       // the seven, in order
pub fn flow(id: &str) -> Option<&'static FlowDef>;          // "agent", "tool", …
pub fn status(ops: &dyn SetupOps) -> Vec<FlowStatus>;       // what's done (cheap, no network)
pub fn form(flow: FlowId, ops: &dyn SetupOps, inputs: &Inputs) -> Form;
pub fn run(flow: FlowId, ops: &mut dyn SetupOps, inputs: &Inputs,
           secrets: &Secrets, opts: &RunOpts) -> Outcome;
```

- `FlowDef { id, title, summary, why, min_role, person_steps }`: `why` is the
  one line behind the (i).
- `Form { preflight: Vec<Check>, steps: Vec<StepDef> }`, `StepDef { id,
  title, fields: Vec<Field>, kind }` with `kind` = `Input | Person | Verify`.
  `Field { id, label, kind: Text | Choice{options} | Secret | Path | Toggle,
  required, default, help }`. Choices are filled from `ops` (harnesses with
  their install and sign-in state, models for the chosen harness, agents for
  a tool's reach). A surface renders one step per screen from this; the CLI
  maps each field to a flag (`--harness`, `--model`, `--name`, …).
- `Inputs`: field id → string (non-secret). `Secrets`: field id →
  `Zeroizing<String>`; `Debug` prints `***`; never serialized.
- `RunOpts { dry_run, driver: Driver::Person | Driver::Agent,
  cancel: Option<Arc<AtomicBool>> }`. A test turn is the agent flow's
  `test-turn` toggle field.
- `Outcome { report: Report, exit: i32, done: bool, results: BTreeMap<String,
  String> }`: `report` is the `ligus.checks.v1` document's body (checks and
  `next`), `exit` follows the shared table, `results` holds non-secret ids
  (the agent id, the runtime id, the repo id).

`run` does, in order: role gate → preflight → validate every field → for each
step, *is it already done?* (skip with an `ok … already` check) → apply →
verify. It stops at the first blocking check. Exit codes:

| Situation | Check | Exit |
|---|---|---|
| done (warnings allowed) | ok / warn | 0 |
| a write or a verify failed | fail | 1 |
| a field is missing or invalid | fail `<flow>.input.<field>` | 2 |
| a tool, a sign-in, the sandbox is missing | fail with the install or login command | 3 |
| a person must act: a passkey approval, pairing, accepting | warn `…pending`, `next` says what | 4 (new for `ligus`; the conventions page and `ligus help exit-codes` say so) |
| the role or lockdown refuses it | fail, fix "ask an Admin" | 6 |
| the relay or the server can't be reached | fail | 7 |

`--dry-run` runs the gate, preflight and validation and reports each step as
`skip "would …"`; nothing is written.

Undo: a step that makes several writes registers an undo for each (`ops`
returns what it created); if a later write in the same run fails, the engine
runs the undos in reverse and reports `…rolled-back`. Writes that sync to
other devices (an agent, a repo) are the last step of their flow, so no undo
of a synced event is ever needed.

Cancel: checked between steps; a cancelled run reports `<flow>.cancelled`
(fail, exit 1) after undoing its own writes.

Synced writes come last and are preceded by every check that could fail
after them (the folder flow runs all of `repos::bind`'s checks before it
adds the repository). The one exception is the CLI's agent flow, where the
workspace runtime and the agent are both synced events: its undo is a
compensating `WorkspaceRuntimeRemoved`, reported as such.

### 3.1 `SetupOps`

Narrow, fact-returning primitives (`Result<_, OpsError>`; `OpsError::
Unsupported` means "not on this surface" and becomes a skip or a person step
that names the other surface):

- context: `surface()`, `my_role()`, `lockdown()`, `workspace_joined()`,
  `relay_url()`, `sandbox()`.
- agent: `harnesses(probe)`, `models(harness)`, `runtimes()`,
  `ensure_runtime(harness, model) -> (id, created)`, `remove_runtime(id)`,
  `agents()`, `member_names()`, `add_agent(name, runtime_id) -> id`,
  `test_turn(runtime_id)`.
- tool: `mcp_servers()`, `mcp_handshake(spec, token) -> tool names` (on an
  unsaved spec, under the agent sandbox), `put_mcp_secret(id, url, token)`
  (records the token's origin, as the desktop's `mcp_bearer_origins` needs),
  `delete_mcp_secret(id, url)` (clears token and origin),
  `add_mcp_server(spec, reach)` (spec and reach in one write),
  `remove_mcp_server(id)`.
- folder: `repos()`, `inspect_folder(path)` (git facts: checkout, remote,
  default branch, root commit), `add_repo(…)`, `bind_folder(repo_id, path)`.
- box: `box_state()` (on a box: identity, joined, pending request),
  `join_request(label)`, `redeem(code)` (the code is a `Secret`),
  `serve_running()`; on a desktop: `hosts()` to verify. `SetupOps` has no
  way to *issue* an admission: no `admit issue`, no recovery words, no
  presence provider (a test checks the trait's surface).
- phone: `phone_precheck()`, `phones()`.
- invite: `invites()` (id, role, created, used).
- relay: `probe_relay(url) -> Probe`, `switch_relay(url)`.

## 4. The flows

Role gates use `WorkspaceRole::rank()`; "none" means any member (or no
workspace yet).

**agent** (Contributor; Admin too when it must define a *workspace* runtime,
the CLI's case). Only runtimes this device may reuse are offered: same
harness, model and endpoint, no inline credential, and (desktop) trusted.
A desktop runtime is device-local, so the agent's turns run on this device,
as agents added in the desktop do today. Steps: harness (claude-code, codex, copilot, agy, aider, pi,
local model / gateway; each shows installed and signed-in, never a token) →
name (rules above) → model (the per-harness list; a free entry for local
models) → sandbox shown as a check (`unconfined` is a warning on the
desktop, and on a box a failure, because the box refuses agents then; the
flow never offers to turn the sandbox off) → optional test turn (the
existing ping, `PINGOK`). The runtime is reused when one with that harness
and model exists; otherwise one is created and undone if the agent write
fails.

**tool** (Contributor once there is a workspace: through shared agents a
tool reaches other people's prompts; refused while the workspace is in
lockdown, i.e. dearth mode, unless the server id is on the lockdown's
`allowed_mcp_servers`: lockdown wins). Steps: transport (`command` + args,
or an `https://` URL; `http://` only for loopback; a URL with userinfo or a
token-like query is refused, and so are args that look like a secret) →
bearer token for a URL server (optional, masked) → handshake on the
*unsaved* spec (`list_tools`, 20 s, under the agent sandbox; an unconfined
host refuses MCP servers; shell tools listed as a warning) → reach (all
agents, or the chosen agents) → one write of the spec with its reach, the
token first (so a failed spec write removes it). Stdio servers get no secret
field in this change: `McpTransport::Stdio` has no env carrier, and a secret
must not land in `args` (argv, the catalog, the trust prompt). Secrets go to
the existing secret store only; the CLI reads them from stdin (`-`) or an
owner-only `@FILE`, never from argv.

Per-agent reach is new and **device-local**: it is kept in this device's
settings (desktop) or `<data dir>/mcp-reach.json` (CLI), never in a catalog
file a commit could change. Hosts copy it onto `McpServerSpec::agents`
(`#[serde(skip)]`, so no catalog can carry it) when they assemble servers,
and `mcp::tool_set` itself takes the turn's agent and drops a server whose
list is non-empty and doesn't name it, so every path (the turn engine and
`ligus reply`) is covered. Fail-closed: a turn with no workspace agent (the
default @ligus) gets only all-agents servers. It only ever narrows; the
egress policy still applies on top.

**folder** (adding a repo to the workspace is Admin; binding a folder to a
repo the workspace already has is Contributor: a binding decides where
turns write on this device). Steps: path → git checks (`inspect_folder`: a
checkout, not a linked worktree, its remote, root commit, the folder not
bound or managed elsewhere: every check `repos::bind` makes) → match or add
the workspace repo (default branch from the one shared detector) → bind.

**box**. On the box (`ligus setup box`, CLI and lt): refuses the desktop
app's data dir; preflight sandbox (Landlock / bwrap; unconfined fails, as
`ligus serve` would) → join request (`ligus admit request`'s output; the
request is public key material, never a secret) → **person step**: an Admin
approves with a passkey on the desktop (exit 4) → rerun with the sealed code
(`--file PATH` or `--code -`, the same as `ligus admit redeem`; the code is
a `Secret` field and never accepted on argv) → verify: joined, and `ligus serve` answering its health
endpoint. On the desktop: Admin gate → hand off to the existing **Admit a
box** (paste, fingerprint, passkey) → verify the host appears. No flag, IPC
call or agent can satisfy the approval.

**phone** (none: it pairs your own phone). Desktop: `phone_enroll_precheck`
as checks → hand off to the existing `AddPhonePanel`, which shows the QR in
the person's own window → verify the phone is listed. CLI: preflight (`ligus
serve` set up, its token present) → **person step** "run `ligus pair` in your
own terminal" (exit 4). The setup command never prints the pairing secret,
in text or JSON.

**invite** (Admin; presence where the workspace requires it). Desktop: role
explained in one line per role → create through the existing invite path
(the code is shown and copied only in the person's own window, as today) →
track: the invite is listed as waiting until its id is in `used_invites`,
then done. CLI: no invite is ever generated (the CLI has no invite path
today and the code is a bearer secret); `ligus setup invite` checks the role,
lists outstanding invites and their acceptance, and hands creation to the
desktop (exit 4).

**relay** (switching this device's relay for a workspace: none; the flow
says every member must switch, and new invites carry the new relay). Steps:
hosted (the community relay preset, `ligus_core::COMMUNITY_RELAY_URL`) or
self-hosted (a URL) → `https://` enforced (plain `http://` only for a
loopback relay on this machine: the literal host `localhost`, `127.0.0.0/8`
or `::1`, never a look-alike such as `localhost.example.com`; no userinfo;
no insecure flag exists in setup) → check (`RelayClient::probe` for this
workspace: `ok` passes, a 404 passes with a warning (nothing stored there
yet), `401` asks for the access token (exit 3), anything else fails and
names `langstroth-relay check --url …`) → switch (desktop: the workspace
list entry and the live settings together; CLI: `config.toml` written to a
temp file and renamed) → verify with a second probe. On an Admin's device
the flow says new invites will carry the new relay. The relay
access token, when needed, is a secret field.

## 5. Surfaces

- **CLI**: `ligus setup` (list), `ligus setup <flow> [flags] [--json]
  [--dry-run]`, `ligus setup runtimes [--apply]`. Every field is a flag; a
  missing required flag is exit 2 naming it (never a prompt). `--json` prints
  one `ligus.checks.v1` document (`command: "setup <flow>"`). `ligus help
  setup` documents every flow, flag, check id and exit code.
- **Desktop**: Tauri commands `setup_flows`, `setup_status`, `setup_form`,
  `setup_run`, `setup_cancel`; one React `SetupWizard` on the kit's `Sheet`
  + `Stepper`: one step per screen, a title and the fields, the explanation
  behind an (i), failures inline as the check's detail and `fix`. Palette
  entries "Set up an agent", "Connect a tool", …; composer `/setup` with
  completion of the flow names; panes get an optional `setupAction` prop
  (minimal, additive, so the in-flight pane-copy rewrite can adopt it).
- **lt**: implements `SetupOps` and renders `Form` in its TUI.

## 6. Security

- **Secrets**: only `Secret` fields carry them; they live in `Secrets`
  (zeroized, redacted `Debug`, no `Serialize`), go straight to the secret
  store through `ops`, and appear in no check, `results`, `next`, log line or
  error. Engine tests feed a canary secret through every flow and assert it
  is absent from the JSON and text output. Pairing secrets and invite codes
  never pass through the engine at all.
- **Privilege**: each flow has a role gate (table in §4) checked before
  preflight, and the underlying write is still authorized by the event fold,
  so the gate is a clear message, not the only defence.
- **Person steps** (passkey approval, pairing, invite creation where presence
  is needed) are `StepKind::Person`; nothing in `Inputs`, a flag or an IPC
  call completes one. The engine only observes their result.
- **Fail-closed defaults**: setup never turns the sandbox off, never accepts
  a plain-http non-loopback relay or MCP URL, never adds an insecure-relay
  flag, never widens an egress or lockdown allow-list, and per-agent reach
  only narrows.
- **Agents driving flows** (a future @ligus "set it up for me"):
  `Driver::Agent` may read `status` and `form`, and `run` refuses it for
  every flow (exit 6) before any spawn, fetch or write. Letting an agent
  run a flow needs a `Confirm` person step before the first side effect and
  writes authorized as the agent (`agent_access`), which is a later change.
  No flow is exposed as a Ligus MCP tool in this change.
- **URLs in errors**: MCP HTTP errors are reported `without_url()` (as the
  relay client does), so a URL never reaches a check's detail.
- **Desktop IPC**: secret fields travel in a separate `secrets` argument
  that the command wraps in `Zeroizing` at once; the wizard keeps them in a
  ref (not state, drafts or storage) and clears them after a run and on
  close.

## 7. Tests

Engine: every flow against a fake `SetupOps` (gate, validation, idempotent
rerun, dry-run writes nothing, rollback on a failing write, cancel, exit
codes, the secret canary, `Driver::Agent` refusals). CLI: the `--json`
shape, exit codes, help coverage. Turn engine: per-agent MCP reach. Web:
vitest for the wizard, palette entries and `/setup` completion. Desktop lib:
the DTO mapping.

## 8. Review (2026-11-24) and what changed

An adversarial review asked for **Revise**. Each finding and where it went:

| Finding | Change |
|---|---|
| Reach stored in a committed catalog could be widened by a commit without re-trust | reach is device-local and `#[serde(skip)]` on the spec (§4 tool) |
| Filtering only in the turn engine misses `ligus reply` | `mcp::tool_set` takes the agent and filters (§4 tool) |
| A server enabled before its reach is written is briefly reachable by all | handshake on the unsaved spec, then one write of spec and reach |
| No env carrier for stdio secrets; URLs and args may carry secrets; `mcp.rs` error leaks the URL | no stdio secret field; userinfo, token queries and secret-looking args refused; `without_url()` |
| Admission codes not typed as secrets | the box flow's code is a `Secret` field |
| `put_mcp_secret` must bind the token's origin | it takes the URL and records the origin; undo clears both |
| Folder: synced repo add before a local bind that can fail | every bind check runs first |
| CLI agent: two synced writes | compensating `WorkspaceRuntimeRemoved`, reported |
| Runtime reuse could pick an untrusted or credentialed runtime | only reusable runtimes are offered |
| `Driver::Agent` could still spawn commands or write | `run` refuses every flow for an agent |
| `admit issue` with recovery words already completes approvals headlessly | `SetupOps` has no issue, recovery or presence method; tested |
| Lockdown with an allow-list is undefined | lockdown means dearth mode, with its `allowed_mcp_servers` |
| Bind and device tools should need Contributor | raised |
| Exit 4 is new for `ligus`; cancel as warn contradicts the table | docs updated; cancel is a failure |
| Loopback http must reject look-alikes; probe needs pass criteria; relay config must be written atomically | defined in §4 relay |

## 9. Security review of the code (2026-11-24)

A prove-then-fix review of the diff found four issues, each fixed with a
test that reproduces it:

| Finding | Fix |
|---|---|
| Claude Code turns get their MCP servers on the command line, past `tool_set`, so reach didn't narrow them | the desktop hands Claude Code only the servers the answering agent may reach (`setup_ui::servers_for_agent`) |
| The relay flow probed a new URL with this device's saved access token (dry run included), and a switch kept the old token | the saved token goes only to the relay it was saved for; switching elsewhere without a token drops it |
| A CLI tool token wasn't bound to its URL, so an edited managed file or a reused id could redirect it | the token's origin is kept in the secret store beside it and checked on every load; removing a server removes its token |
| A stray argument (a pasted admission code) was echoed in the usage error | it is no longer shown |

# Workspace model v3 (design note)

Status: design, approved by the owner on 2026-11-26 (decisions in section 19);
being implemented for Ligus 0.5. The implementation plan is
[`workspace-model-v3-plan.md`](workspace-model-v3-plan.md).
Surfaces: the desktop, `lt` (ligus-term), the `ligus` CLI, `ligus serve` and
`ligus worker`, the phone app, and one additive relay route
(`langstroth-relay`). Ligus is short for *ligustica*, the Italian honeybee.

This note follows [`identity-model-v2.md`](identity-model-v2.md), which
shipped in 0.4 and fixed the identity layer: profiles, device labels, the
per-workspace binding, `MemberRetired`, phone consent, rules version 3, seeds
out of SQLite and sync state per relay. This note covers everything else:
agents, runtimes, hosts and model gateways; who a box or a phone acts for;
devices, phones and pairings; the event log's local invariants; the
lifecycles of messages, turns, asks, proposals, attachments and scratch
repositories; local stores on every surface; and the relay.

Citations are files and functions in this repository at `origin/main`
`5312f9715` (0.4.1). `core/` is `crates/ligus-core/src/`, `rt/` is
`crates/ligus-runtime/src/`, `cli/` is `crates/ligus-cli/src/` and `app/` is
`app/src/`. When this note and the code disagree, the code wins.

The direction extends v2's "one source of truth per concept" to the rest of
the model: **every fact has one authority, every reference is typed and
checked, every entity has a way out, and every request knows whose it is.**

## Contents

1. What is wrong today
2. Principles
3. Compatibility classes
4. Entities and authorities
5. Agents, sources, hosts and placement
6. Principals: who a box and a phone act for
7. The event log: order, pruning, dispatch
8. Messages and turns
9. Asks, proposals, attachments, scratch repositories, channels
10. Devices, labels, pairings, push and member exits
11. The workspace layer
12. Lifecycles
13. Local storage
14. The relay
15. Rules version 4
16. Migrations and mixed versions
17. The onboarding model this enables
18. Invariants and their tests
19. Owner decisions
20. Threats and residual risks

## 1. What is wrong today

**In short:** an agent doesn't say which model it uses, so one id means
different models on different machines. Where an agent runs is a second step
that can fail silently. A box decides "is this for you" with its own
identity. Three different row orders exist in each local database, and some
readers pick the wrong one. A reply is stamped with its turn's start. Many
things (hosts, devices, service members, pairings) have no way to leave.

### 1.1 Agents name a runtime, not a model

- `WorkspaceAgent` (`core/agent.rs`) has a `runtime_id` and no model. The
  model is whatever that id resolves to on the device that runs the turn.
  `host_id = ""` means "the owner's device".
- `runtime_id` has two namespaces. The desktop resolves it only in its
  device-local catalog (`resolve_runtime`, `app/chat_models.rs` and
  `app/lib.rs`) and never reads `WorkspaceRuntime`. Workers and boxes resolve
  workspace runtimes, then their `runtimes.toml`, then an `id = "*"`
  catch-all (`cli/worker.rs`). The CLI's `ligus agent` reads workspace
  runtimes only.
- `WorkspaceRuntime` (`core/workspace_runtime.rs`) and the device-local
  `RuntimeTarget` (`core/runtime.rs`) each fix **one model**. Setting up an
  agent reuses a runtime only when harness, model and endpoint all match, so
  every model needs its own runtime, and a workspace runtime needs an Admin
  with a passkey.
- The desktop silently falls back to the Claude Code CLI when it meets an
  unknown runtime id or a hosted provider with no key.
- A host's gateways come from a host-local file
  (`LIGUS_MODEL_GATEWAYS_FILE`, `rt/model_gateway.rs`). Desktops publish no
  offers. `offers_gateway` exists but nothing checks it when an agent is
  bound, so a wrong binding fails at turn time.
- Model server addresses (`WorkspaceRuntime.endpoint`) are on the log, where
  every member, Viewers included, can read them. Gateways already avoid this.
- The default `@ligus` agent has no record, no id on its replies and no run
  settings of its own.

### 1.2 Where an agent runs is a second, silent decision

- 0.4.1 (#281) writes the host in the same record when an agent is added
  from the Tools pane, but the authz floor (`agent_change_floor`,
  `core/authz/mod.rs`) still makes any new agent with a host an Admin
  action, so a Contributor can't put their own agent on their own worker.
- Dispatch (`host_binding`, `turn_runs_here`, `rt/turn_dispatch.rs`) skips an
  agent bound elsewhere. 0.4.1 (#282) shows one line when the bound host is
  offline. When it is offline long enough, the sender's device still answers
  locally with **its own** catalog, login and the Claude Code fallback.
- A Contributor can register a host record under someone else's device id:
  the fold checks that `owner_actor_id` is the author, not that the host id
  is the signing device (`core/authorization.rs`, the `SelfHost` rule).

### 1.3 A box decides "you" with its own identity

`ligus serve` is its own Service member. Some routes already act for the
phone's person (`/v1/me`, proposal viewer fields, the event stream, scratch,
phone actions; #276 for question pushes; #283 for DM listing and proposal
notes). Still computed for the box: unread counts (so a person's own phone
posts count as unread to them), read marks (one map per box), agent
availability, transcript "mine", "may clear lockdown", the hourly question
cap and ntfy. The direct-path (LAN) bearer is one token shared by every
direct-path phone and falls back to the box's own view. A phone the box can't
place falls back to the box's identity for pushes (#276), which is the
opposite of failing closed.

### 1.4 Three orders, and pruning without memory

- Fold order is `(lamport, event_id)`. Several readers use the database row
  order or the local `sequence` to mean "newest": `latest_contact_of` and
  `messages_after` in `rt/event_store.rs`, snapshot cadence, `list_in` and
  `list_after`. #277 fixed the signer-chain query (`LAST_SIGNED_BY_SQL`).
- `prune_superseded_chunks` deletes streaming chunks of finished messages on
  **every** desktop launch (`app/lib.rs`). Ingest checks only the `events`
  table, so a pruned chunk fetched again after a sync cursor reset is stored
  again as if it were new.
- Turn takeover reads the payload's `created_at` (`core/dispatch.rs`,
  `TAKEOVER_MAX_AGE`), a clock the author chooses.
- Non-seed `SessionSnapshot`s are written every 200 rows, signed, synced and
  stored, but they are inert in the fold.

### 1.5 Replies and turns have no lifecycle

A reply starts as an empty streaming `MessageAppended` at turn start
(`begin_assistant_message_with_model`, `rt/chat_service.rs`). That
placeholder is also the dispatch commit, the turn id, the worktree name and
the anchor for questions. `MessageCompleted` carries no outcome, so a failed
or abandoned turn (the stale-stream sweep writes an ordinary completion)
looks like a short reply. #275 added the display time from the completion.
There is no turn entity: a turn is a trigger id, claims, dispatch slots, flow
records and in-memory waiters.

### 1.6 Things with no way out

Device labels live in six places, and a box writes its `--label` or hostname
on **every** start, reverting a rename made on the desktop. Service members
can only be removed by an Admin. Host Clean up exists in the runtime
(`list_all_hosts`, `remove_host`) but has no UI. A computer approved but
never redeemed can only be revoked. A phone pairing is cut when its
certificate is revoked, but not the other way round: unpairing on the box or
forgetting the box on the phone leaves the certificate live. Revoked pairing
records are never removed.

### 1.7 Already fixed in 0.4.1

| Problem | Fixed by |
|---|---|
| Every paired phone listed every DM the box holds | #283 (the tunnel path; the shared direct-path bearer is section 6) |
| Proposal notes from a phone were written as the box | #283 refuses them until notes are phone-signed |
| A worker ran an unknown runtime on its `*` entry without saying so | #283 says so in the chat; section 5.4 refuses full run specs there |
| A parse error erased the scratch registry | #283 moves the bad copy aside |
| serve ignored the phone's notification kinds | #283 |
| An agent's reply was stamped with its turn's start | #275 |
| A refused draft didn't send again | #277 |
| A new agent's host was a second write | #281 (Tools pane) |
| An agent on an offline host was skipped with no trace | #282 (one line) |
| The CLI's passkey refusal pointed at `ligus doctor` | #284 |

Still open and folded into 0.5: host records under another device's id
(5.1), takeover age by the payload clock (7.3), the prune on every launch
(7.2), and everything per person on the box (6).

## 2. Principles

**In short:** one authority per fact; references are checked, and a broken
one shows as a state; everything has an exit; every action knows its
principal; order and time come from signed fields.

| # | Principle | Consequence |
|---|---|---|
| P1 | **One authority per fact.** Every other copy is a named cache with a named refresh trigger. | Each section says what is authoritative and what is a cache. |
| P2 | **Typed references with visible integrity.** A reference is checked when it is written. A reference to something gone renders as a state ("host removed", "source not offered"), never as a fallback to something else. | No silent Claude Code substitution; no empty string meaning two things. |
| P3 | **Every entity has a lifecycle**: created, renamed, retired or left, revoked or removed, each by an event or a defined fold rule. "Leftover" is not a state. | Section 12. |
| P4 | **Ask, offer and choose are three records.** What the workspace asks for (synced intent), what a host offers (host record and beats), what a device chose locally. Dispatch resolves them and records the outcome. | Section 5. |
| P5 | **Every action has a principal** `(person, device, via)`. Code that decides "is this for you" receives the principal and never reads the process's own identity. | Section 6. |
| P6 | **Order and time come from signed envelope fields.** Order is `(lamport, event_id)`; display time is the signed timestamp of the event that made a thing visible. Payload clocks are hints; row ids are storage keys. | Sections 7, 8. |
| P7 | **Local derived state is rebuildable** from the stored log plus named local inputs. Deleting a stored row leaves a tombstone. | Section 7. |
| P8 | **No secrets and no host-owned URLs on the log.** The log carries names and kinds; hosts resolve them. Model keys are never in the workspace, not even encrypted. | Section 5.5. |
| P9 | **Compatibility classes** from v2 stay: R render-only, L local-only, G rule-gated. G now means rules version 4. | Section 3. |
| P10 | **Upsert records grow sticky fields.** A field added to an upsert record keeps its previous value when an upsert omits it, so an older writer's whole-record upsert can't erase it. Clearing is explicit. | Section 5.6. |

## 3. Compatibility classes

**In short:** the same three classes as v2. New display data and local
changes ship freely; anything that changes what the fold allows waits for
rules version 4, which older builds treat as read-only.

- **Class R** (render-only): new events or optional fields whose fold output
  only feeds display, such as `MessageCompleted.outcome`. Older builds
  decode them as `Unknown` or ignore the field, store the bytes unchanged and
  re-push them.
- **Class L** (local-only): dispatch, local storage, how a box picks the
  person behind a phone, render-time resolution. Nothing on the wire changes.
- **Class G** (rule-gated): every change to what the fold allows or denies is
  behind **rules version 4** (`RULES_SUPPORTED`, `core/authz/mod.rs`). Until
  a workspace is on rules 4, a 0.5 build folds exactly as 0.4 does, and gated
  events are inert everywhere. Once rules 4 is set, a 0.4 build makes the
  workspace read-only on its device (the existing fail-closed path) and shows
  "update Ligus".

The v2 obligations stay: no wire id is renamed; new kinds and optional fields
only (`serde(default)`, skipped when empty); no new kind is refused at
ingest; new `ChatSession` fields are `skip_serializing_if` empty; nothing new
goes into a phone-drafted preimage unless the phone says it reads it.

## 4. Entities and authorities

**In short:** a person has devices; a device may be a host; a host offers
sources by name; an agent asks for a source and a model and says where it
may run; a chat holds messages, and a reply has a turn. A principal is
derived per request and never stored.

```
Account(person) 1─N Device ─(certificate, accepted)─ Workspace
   │                 │
   │                 ├─0..1 Host (id = device id) 1─N Source (offers: names and kinds only)
   │                 │                            └─ Beat (sealed, not on the log: online,
   │                 │                                     readiness, model lists, build, refusals)
   │                 └─0..N Phone pairing (per box, transport state)
   │
Workspace 1─1 Config log ─┬─N Member ── Account | Service account (a box)
                          ├─N Agent ── run {harness, source, model} ──▶ Source (by name)
                          │          └ placement {ownerDevices | host(id) | pool | chatCreator}
                          ├─N Host, Repo, Channel, Profile, DeviceLabel, Rules, Guards, Memory
                          └─N Chat ─1─1 Chat log ─┬─N Message ─0..1 Turn ─┬─N Ask
                                                  │                        ├─N Proposal ─N Apply
                                                  │                        └─0..1 Worktree (host-local)
                                                  ├─N Attachment ──▶ Relay blob
                                                  └─0..1 Chat repo ──▶ Repo ─0..N Scratch repo (host-local)

Principal = (person, device, via host?)   derived per request, never on the log
```

| Fact | Authority | Caches, and when they refresh |
|---|---|---|
| What an agent runs | `WorkspaceAgent.run` (rules 4), else its `runtime_id` | `run.resolved` display hint, written by the owner's device |
| Where an agent may run | `WorkspaceAgent.placement`, kept consistent with `host_id` | none |
| What a host can run | the host's own configuration (`host.toml`, the gateways file) | `offers` on its host record, rewritten when the configuration changes |
| Whether a host is online, ready, its model lists and build | the sealed host beat (every 30 s, online within 90 s) | `last_seen` on the host record, written only when the record changes for another reason |
| A device's name | `DeviceLabelSet` (v2) | host label, `PhoneRecord.name`, lt's `device-label` file, the relay's label column (ignored) |
| Who a request is for | the principal (section 6) | the single-holder pin for a phone, in `principal_pins` |
| What a turn did | `MessageCompleted.outcome` (author-asserted, display only) | none |
| Read marks, Hide for me, per-chat model choice, push preferences | the device (0.5); the person's private state log (0.6) | none |

## 5. Agents, sources, hosts and placement

**In short:** a host lists what it can run (Claude Code signed in, a gateway,
a local model server) by name. An agent says which of those it wants, which
model, and where it may run. Dispatch matches them and always shows the
result in the chat: running, waiting for a host, or can't run and why.
Nothing silently runs somewhere else or with another model.

```
Host (device) ── offers ──▶ Source {name, kind, harness | wire}     [host record: names only]
   │ beats (sealed): online, ready, models[], build, refuses[]
   ▼
Dispatch ◀── asks ─── Agent.run       = {harness, source, model}
         ◀── where ── Agent.placement = ownerDevices | host(id) | pool | chatCreator
         ──▶ TurnRoute (shown in the chat): RunsHere | RunsOn(host) | Queued(hosts) | Unrunnable(why)
```

### 5.1 What a host offers

An additive, sticky `offers` list on `WorkspaceHost`, a superset of
`model_gateways` (which writers keep filling for older readers):

```jsonc
"offers": [
  { "name": "claude-code", "kind": "cli",      "harness": "claudeCode" },
  { "name": "codex",       "kind": "cli",      "harness": "codex" },
  { "name": "ollama",      "kind": "local",    "harness": "ollama" },      // the URL stays on the host
  { "name": "gw-home",     "kind": "gateway",  "wire": "openai" },         // = a model_gateways entry
  { "name": "team-claude", "kind": "provider", "harness": "anthropic" }    // a secret the host holds
]
```

- `name` matches `[a-z0-9-]{1,63}` (the gateway rule); at most 32 offers. No
  URL, account, plan or secret appears anywhere. `kind`, `harness` and `wire`
  come from the host's own configuration, never from a member.
- **A host resolves only names it declared.** A `provider` offer maps to a
  secret that the host's `host.toml` lists for this workspace. The host never
  builds an environment variable or secret id from a string on the log.
  (Today `SecretRef` builds `LIGUS_WS_SECRET_<ref>` from the log string,
  `core/workspace_runtime.rs`, so an Admin could point a runtime at a secret
  meant for another workspace. Under v3 a legacy `SecretRef` resolves only
  when `host.toml` lists it for this workspace.)
- **Who publishes.** Workers and boxes always. A person's own computers
  (desktop, lt, CLI) publish by default, so the one-step agent sheet can list
  "Claude Code · signed in · this Mac" and "codex · build-1" for every host
  the person can place an agent on. Settings has "Don't list this computer's
  tools".
- **Readiness and model lists are not on the log.** Per offer, the sealed
  beat carries `ready` (installed and signed in), up to 64 model names and a
  `complete` flag, refreshed at most every 5 minutes (the lists of
  [`chat-model-picker.md`](chat-model-picker.md)). The beat also carries the
  host's build version. A beat over the relay's presence limit drops the
  lists first.
- **A host record is bound to its device.** It counts only when it is signed
  by the device whose id it carries, that device is trusted (accepted
  certificate, not revoked), and `owner_actor_id` is an account that holds
  the device. In 0.5 readers and pickers ignore any other host record (class
  L); rules 4 denies it (G13).

### 5.2 What an agent asks for

Additive, sticky fields on `WorkspaceAgent`:

```jsonc
"run":       { "harness": "codex", "source": "codex", "model": "gpt-5.3-codex" },  // model "" = the source's default
"placement": { "kind": "host", "hostId": "<device id>" }   // or ownerDevices, pool, chatCreator
```

- **A full run spec is a rules-4 feature.** Under rules 3 any change of
  `runtime_id` is Admin plus presence (`core/authz/rules.rs`), so writing a
  model-specific id there would make every model change an Admin action.
  Therefore:
  - **rules 3**: `run` only mirrors the existing runtime, `{source:
    "runtime:<id>"}`, plus a display hint `resolved: {harness, model}` from
    the writing device. The model still belongs to the runtime, and the
    agent sheet reuses or creates one (5.9). A model change is Admin plus
    presence, and the UI says so;
  - **rules 4**: `run = {harness, source, model}`, and `runtime_id =
    legacy_id(run)`.
- `legacy_id(run)` is exact and reversible: `runtime:<id>` maps to `<id>`;
  any other spec maps to `run:` + base64url of the canonical JSON
  `{"s": source, "m": model}`. Model names with `:` or `/` (`llama3:8b`,
  `org/model`) round-trip, and no catalog id collides with it.
- `host_id` stays and is always written consistently: `host(id)` gives `id`,
  `ownerDevices` gives `""`, `pool` gives the pool's first host at write time
  (so older devices route somewhere real).
- `WorkspaceRuntime` records stay readable forever. New UI stops creating
  them; `ligus add-runtime` stays as an expert command for workers that need a
  preset, and an agent may still name one (`run.source = "runtime:<id>"`),
  resolved exactly as today.

### 5.3 Placement

| Placement | Who runs a turn | When nobody can |
|---|---|---|
| `ownerDevices` (default for a personal agent) | any online device of the agent's owner **that offers the source**; the turn claim picks one | `Queued(owner's devices)`, visibly in the chat; never another person's device |
| `host(id)` (default for an always-on agent) | exactly that host | `Queued(host)`, with "Run here this time" for the agent's owner (and Admins on shared agents) when this device offers the source |
| `pool` (rules 4 only) | any online worker offering the source, lowest device id first; the claim arbitrates | `Queued(pool)` |
| `chatCreator` (the default agent only) | the chat creator's devices, as today | `Queued` |

- **A host that is gone is a state, never a fallback.** When the bound host is
  removed, revoked, left, or owned by a former member, the agent is
  `HostGone` and runs nowhere until someone places it again. It never turns
  into `ownerDevices`. The rules-3 post-pass clears `host_id` for hosts of
  former members (`core/events.rs`), which a placement-aware reader would read
  as "owner's devices", so the fold also records `host_gone: true` and readers
  show "box-1 was removed. Choose where @x runs."
- **Agent keys follow the placement** (rules 4, G14). Today an agent key is
  accepted only from the bound host, or from the owner's devices when unbound
  (`agent_bound_to`, `core/authz/timeline.rs`; `authorize_agent`,
  `core/authz/mod.rs`). Under rules 4 a key certified by any host the
  placement admits is accepted (for `pool`: workers whose valid host record
  offers the source), plus the device a `TurnDelegated` names for that turn.
- **"Run here this time" is a signed one-turn delegation** under rules 4: a
  new chat-log kind `TurnDelegated{triggerId, agentId, deviceId}`, written by
  the agent's owner (or an Admin for a shared agent), allows exactly one turn
  on that device, with the agent's own run spec. Under rules 3 it stays as
  today: only the owner's own devices can answer an unbound agent.

### 5.4 The dispatch outcome

Every device computes the same route from the same inputs; only the
sender's device shows it (class L).

```rust
enum TurnRoute {
    RunsHere { source, model },
    RunsOn { host, model },                 // "@x on build-1 · codex / gpt-5.3-codex"
    Queued { on: Vec<HostId>, since },      // a quiet row in the chat
    Unrunnable(Why),                        // a row with one action
}
enum Why { HostGone { removed | revoked | left | ownerGone }, SourceNotOffered { host, source },
           ModelNotOffered { model }, NotAllowedOnHost, OwnerGone, NoKeyHere }
```

- `Queued` and `Unrunnable` render as one quiet row under the message: "@x
  runs on build-1, which is offline. It will answer when build-1 is back. ·
  Run here this time". Never a log line only.
- **No silent substitution.** An unknown source, a missing key, or a model
  that a complete list doesn't have is `Unrunnable` with its reason. The
  Claude Code fallback becomes an explicit "Use Claude Code instead" for that
  one turn, recorded in its outcome.
- **The offline-host fallback that answers on the sender's device is
  removed.** "Run here this time" is the only way, with the agent's own run
  spec, and only when this device offers the source.
- **A worker never runs a full run spec it can't resolve.** The `*`
  catch-all applies only to legacy runtime ids; a `run:` id it doesn't know
  is `Unrunnable(SourceNotOffered)`. A 0.5 client never places a full run
  spec on a host whose beat reports a build below 0.5.
- **Host-side policy** (host-local, never on the log): a desktop runs only its
  own person's agents unless the person opts in; a worker or box runs any
  agent placed on it. A host that refuses a placed agent says so in its beat
  (`refuses: [agent ids]`), and readers show `NotAllowedOnHost`.

### 5.5 Gateways, local servers and provider keys

- The operator's gateway allow-list stays host-local. `ligus gateway add`
  writes it to a default file in the data dir (section 17.7); the environment
  variable still wins.
- The workspace sees a gateway only as an offer `{name, kind: gateway,
  wire}`. An agent asks for `run.source = <gateway name>`, and its harness
  comes from the offer's wire (`openai` gives OpenAI-compatible, `anthropic`
  gives Anthropic Messages), never from a member's choice.
- Placing an agent on a host whose offers don't include its source is refused
  by the client in 0.5 ("build-1 doesn't offer gw-home") and denied by the
  fold under rules 4 (G2).
- A gateway-exclusive host lists only its gateways as offers, so the picker
  can't offer anything else there.
- New local model servers are offers of `kind: local` with the URL in the
  host's own config. New hosted providers are offers of `kind: provider`
  backed by the host's secret store. The log carries neither URL nor key.
- Existing `WorkspaceRuntime.endpoint` values keep working. Admins see "This
  runtime's address is visible to every member. Move it to the host that runs
  it." with one action that writes the offer and repoints the agents. Rules 4
  denies adding or changing an endpoint (G9); a rename that keeps it is
  allowed.

### 5.6 Sticky fields and older writers

An older client that renames an agent sends the whole record without `run`,
`placement` or `offers`. On 0.5 builds the fold applies these rules:

1. A field absent from an upsert keeps its previous value.
2. If the upsert's legacy field disagrees with the sticky field it shadows
   (`host_id` isn't the host the placement implies, or `runtime_id` isn't
   `legacy_id(run)`), **the legacy field wins** and the sticky field is reset
   to what the legacy field means. An older writer that moved an agent
   therefore moves it for everyone.
3. Clearing is explicit: `placement: {kind: ownerDevices}`, `run.model: ""`.
4. `AgentRemoved` and a whole-list `AgentRosterUpdated` drop the sticky
   fields with the record, so an id created again starts clean.
5. **Authz judges the merged record.** The placement and run rules and the
   presence rules are evaluated on the effective record after rules 1 to 4,
   never on the payload alone, and any change of the effective `host_id`,
   placement, `runtime_id` or run source counts as a move. (Otherwise a
   Contributor could send a 0.4-shaped upsert with `host_id` set to someone
   else's worker and rule 2 would move the agent without an Admin.)

In the fold the sticky fields are `Option`s, so "absent" and "explicitly the
default" differ. Canonical order makes the result the same on every 0.5
build; older builds read only the legacy fields, which rule 2 keeps
consistent.

### 5.7 Who may create and move agents (rules 4)

| Change | Floor | Passkey |
|---|---|---|
| Create an agent you own, placed on `ownerDevices` | Contributor | no |
| Create or move your agent to **a host you own** | Contributor | yes, one ceremony, the same event |
| Create or move any agent to someone else's host, a worker you don't own, or `pool` | Admin | yes |
| Change `run.model` within the same source, on your own agent | Contributor | no (the agent's limits still apply) |
| Change `run.source` or `run.harness` | as moving the agent | yes |
| Change someone else's agent | Admin (unchanged) | as today |

Under rules 3 these stay as they are today: a model change is a runtime
change (Admin and passkey), and a new agent with a host is an Admin action.

- **G2**: an `AgentUpserted` whose placement names a host is denied unless
  that host is in the projection at the event's position, its device isn't
  revoked, its owner is a member, and (when the host published offers) the
  source is among them. This is sound in canonical order: an author who saw
  the host record stored it, so their own event's lamport is higher
  (`core/lamport.rs`).
- **G3**: a host whose device is revoked, or whose service member left, is
  inert, extending the rules-3 rule for hosts of former members. Its agents
  become `HostGone`.
- **G13**: a `WorkspaceHostUpserted` is denied unless it is signed by the
  device whose id it carries, that device is trusted at that position, and
  the owner holds it. This also gives "a host you own" a real meaning.

### 5.8 The default agent (rules 4)

The default agent gets a well-known id, `UUIDv5(workspace_id,
"ligus-default-agent")`, written as `responder_id` on its replies, and an
optional record (`AgentUpserted` with that id, name locked to "Ligus") with a
`run` and the placement `chatCreator`, so its model is set like any agent's.
The id is reserved and known to `authorize_agent` without a record (today an
unknown `responder_id` is denied). Its replies are signed as today, by the
chat creator's device. `SessionRuntimeChanged` can still override its run per
chat. Before rules 4 nothing changes: replies keep `responder_id: None`. The
device-local per-chat model choice (`chat_models`) stays until the person's
private state exists (0.6).

### 5.9 Bridging rules 3

On a rules-3 workspace the one-step agent sheet (17.4) still asks for a
model per agent. Underneath, the setup engine reuses a runtime with the same
harness, model and endpoint or gateway, or creates one. A source that needs a
workspace runtime (a secret reference or a gateway) then takes two passkey
confirmations in the same sheet, shown as "Confirm 1 of 2". On rules 4 the
agent carries its run spec, so there is one event and at most one
confirmation.

### 5.10 Liveness

The sealed host beat (30 s, online within 90 s, `rt/host_presence.rs`) is
the only authority for "online". `last_seen` on the host record is written
only when the record changes for another reason; serve's `touch_host` for
contacts moves to the beat. `WorkspaceHost::is_online` takes the one
constant `HOST_ONLINE_WINDOW_SECS`, not a caller's window.

## 6. Principals: who a box and a phone act for

**In short:** every request through a box is made for a person, the one
behind the phone that sent it. The box uses its own identity only for its
own records. When the box can't tell who a phone belongs to, the phone gets
workspace-wide notices without previews, and nothing personal.

```rust
// rt/principal.rs (new, not on the wire)
pub struct Principal {
    pub person: Who,             // the person, or the box's own service account
    pub device: Uuid,            // the device that made the request
    pub via: Option<Uuid>,       // the host relaying it (the box), if any
    pub standing: Standing,      // Member(role) | ReadOnly(reason) | Unresolved
}
pub fn principal_for_phone(ws, phone_device) -> Principal   // phone_person, else Unresolved
pub fn principal_for_self(ws) -> Principal                   // me(ws) from binding()
```

- Every serve route, event stream, push decision, unread count, read mark and
  question cap takes a `Principal`. The box's own identity is used only for
  its host record, its device label, its service-member writes, the agent
  turns it hosts (agent keys) and its ntfy channel.
- **Unresolved fails closed**: workspace-wide notices only (proposals, gates,
  join requests), without previews, and no "you" notices. This replaces
  #276's fallback to the box's identity.
- **Per-person state on the box** is keyed by the person's account:
  `readMarks: {account: {chat: ms}}`, the question cap per (person, hour), and
  unread "mine" means authored by that person on any of their devices, not by
  the box.
- **Actions are the phone's, or refused.** Anything a phone request writes is
  a phone-signed `PhoneAction` drafted for its principal. No route writes as
  the box on a phone's behalf. Listing is filtered by principal (workspace
  chats, plus only the DMs the person is in), and agent availability and
  transcript "mine" are computed for the principal.
- **Direct path.** A phone on the direct (LAN) path authenticates per phone:
  its P-256 key signs a per-request challenge, the tunnel's credential model.
  A request with only the shared bearer is `Unresolved`.
- A desktop's principal is `me(ws)`; a linked computer's is its `binding()`
  person. lt and the CLI already go through `binding()`.
- A source-level test (like v2's I10) checks that no "you"-dependent function
  in `cli/serve` reads `my_actor_id()` or `svc.author()`.

## 7. The event log: order, pruning, dispatch

**In short:** "newest" always means highest `(lamport, event_id)`. A pruned
event leaves a tombstone, so fetching it again doesn't bring it back as new.
Dispatch remembers which messages it has handled instead of guessing from
position or clocks.

### 7.1 Order

Every "latest", "newest" and "after" query is by `(lamport, event_id)` within
a log, never by `row_id` or the local `sequence`. `row_id` is a storage key.
A lint test lists the SQL statements that may order by `row_id` (prune scans,
migrations) and fails on any other.

### 7.2 Tombstones and fewer chunks

- A new table `pruned_events(workspace_id, session_id, event_id,
  envelope_digest, lamport, signer, reason, pruned_at)`. Ingest checks it
  before storing; a re-fetched copy with the same digest is recorded as a
  witness only. A **different** copy under the same id isn't hidden by the
  tombstone and goes to `equivocations` as today. The table is backfilled
  from `lamport_witnesses` rows that came from pruning.
- Writers coalesce streaming chunks: at most one every 2 seconds or per 4
  KiB, with the existing secret hold-back.
- The prune runs at most daily, never on the launch path, and only behind
  tombstones.

### 7.3 Dispatch novelty

Novelty means **"this device hasn't processed this event id"**, not a lamport
watermark: a message written offline at lamport 51 can arrive after the chat
reached 80 and must still be answered. A table `dispatch_processed
(workspace_id, session_id, event_id, processed_at)` plus the tombstones
decides it. An age bound (24 hours) on the event's **envelope** timestamp,
not the payload's `created_at`, stops very old re-fetched messages from being
taken over. The takeover age in `core/dispatch.rs` moves to the envelope
timestamp too. Rows older than the bound are trimmed.

### 7.4 Snapshots and place

- Stop writing non-seed snapshots. The seed snapshot at chat creation stays;
  a joiner gets history from the relay.
- The `place` split (v2's M5: a `place` column, the signed `sequence` drafted
  per signer and log, `UNIQUE(session_id, sequence)` dropped) is 0.6.

## 8. Messages and turns

**In short:** a reply is a draft while it streams and then becomes posted,
failed or canceled, and says which model answered. It is shown where it
finished, at the time it finished. A turn becomes a named thing that the
code can ask about.

### 8.1 The message lifecycle

```
Draft (placeholder, streaming) ──MessageCompleted{outcome: ok}───────▶ Posted
        │                      ──MessageCompleted{outcome: failed}───▶ Failed (body = a readable error)
        │                      ──MessageCompleted{outcome: canceled}─▶ Canceled (stale sweep, user stop)
        └── MessageRemoved ──▶ Removed (from any state)       Superseded: fold state (dispatch)
```

- `MessageCompleted` gains an optional `outcome` (class R):
  `{status: ok | failed | canceled, harness?, source?, model?, host?,
  tokens?}`. It is asserted by the author, display only, and labelled
  "reported by <host>". A `canceled` written by the stale-stream sweep is
  overridden by a later completion of the same message from the
  placeholder's own signer (another device of the same account may still have
  been streaming).
- **Display time** of a reply is the signed timestamp of its completion
  (#275's `posted_at`, generalised); of anything else, its own envelope time.
  Payload `created_at` is a fallback only.
- **Transcript order**: a posted reply is drawn at its completion's canonical
  position; a streaming reply is a live row at the end of the transcript. The
  fold's own order is unchanged.
- **Live text stays on the log.** Chunks remain `MessageChunkReceived`,
  signed by the reply's agent key (only that key may write them,
  `core/authz/agent.rs`), which phones (serve builds stream deltas from the
  folded body), lt and the crash sweep depend on. Coalescing (7.2) makes a
  reply a handful of events instead of hundreds. Moving live text to relay
  presence was considered and is not adopted: presence slots have no expiry
  or delete, a room holds a bounded number of slots with arbitrary eviction,
  and beats are unsigned, so any member could inject "agent" text.

### 8.2 Turns

A `Turn` view in `ligus-core`, derived from the chat fold (class L, no new
event):

```rust
pub struct Turn {
    pub id: TurnId,                   // = the placeholder message id (unchanged)
    pub trigger: Uuid, pub agent: Who,
    pub claim: Option<(DeviceId, u32)>, pub host: Option<HostId>,
    pub route: Option<TurnRoute>,     // local only, from dispatch (5.4)
    pub state: TurnState,             // Queued | Claimed | Running | WaitingOnAsk
                                      // | Posted | Failed | Canceled | Superseded
    pub asks: Vec<AskId>, pub proposals: Vec<Uuid>, pub outcome: Option<Outcome>,
}
```

`TurnId(Uuid)` is a newtype everywhere it is used (asks, worktrees, MCP
handles, facts). `WaitingOnAsk` comes from an open question whose `turn_id`
is the turn; the host's restart sweep closes both.

## 9. Asks, proposals, attachments, scratch repositories, channels

**In short:** small changes. Question eviction stops trusting the author's
clock, the scratch registry becomes a real table, and filing a chat into a
channel follows the workspace role.

- **Asks.** The 0.4 lifecycle stays (Open, then Settled, Closed or the
  Expired view), now hung off `Turn`. Eviction (256 per chat) orders by the
  ask's canonical position under rules 4 (G10), not the payload's `at_ms`.
  Waiting becomes a notification on fold change instead of a 250 ms poll.
  Push and "for you" use the principal.
- **Proposals.** No event renames. `ProposalDismissed` is documented as
  "archived for everyone" in the fold's typed view. Hide for me stays
  device-local until the person's private state (0.6). The apply target
  references typed `HostId` and `RepoId`.
- **Attachments.** Unchanged. 0.5 adds the notice for Admins when a room's
  blobs reach 80% of the relay's limit. "Gone" stays device-local; a device
  learns it from a 404.
- **Scratch repositories.** The registry moves from one JSON blob in
  `repo_settings` to tables `scratch_repos` and `scratch_repo_chats`. The
  blob is imported once and kept as a backup; a parse failure stops the
  import with a `ligus doctor` warning.
- **Channels.** No model change; `ChannelId(String)` in code. Under rules 4
  `ChatChannelChanged` is judged by the workspace role (Contributor, G12),
  with the creator rule kept for non-members of the chat.

## 10. Devices, labels, pairings, push and member exits

**In short:** a device has one name, set once and changed only on purpose. A
phone pairing belongs to a device, and ending either one offers to end the
other. Boxes and people can leave a workspace on their own.

### 10.1 Labels and device states

- `DeviceLabelSet` is the authority. `WorkspaceHost.label` is written from
  it and read only as a fallback; the relay's label column is ignored.
- A box writes its label only when `--label` or `LIGUS_SERVE_LABEL` changed
  since the last label it wrote itself (kept as `last_self_label` in its data
  dir). With neither set, it writes the hostname once, on first start. A
  rename on the desktop survives a restart.
- lt rewrites its `device-label` file when a synced label for itself is newer
  than the one it wrote.
- At phone enrolment the certifying desktop writes `DeviceLabelSet` for the
  phone from the name the phone chose, in the same commit, with no extra
  ceremony. A phone is never "Device 376b5d72".
- My devices and People show **Pending** (approved, nothing written),
  **Active**, **Idle** (no signature and no beat for 30 days, a view) and
  **Revoked**. Pending and Idle devices get **Remove** (the existing
  `DeviceRevoked`, with a passkey).

### 10.2 A pairing is transport state attached to a device

| Record | Authority for | Keyed by |
|---|---|---|
| Device certificate (on the log) | that the phone is the person's device | device id |
| `PhoneRecord` (on the box) | how this box reaches and pushes to it | (box, device id); `pid` is the transport id |
| `PhoneBinding` (on the phone) | a cache of `/v1/me` | (host, workspace) |

- New pairings must name their device (`deviceId` required). Older ones
  without it show as "unverified pairing" and get no "you" notices.
- **Both directions:**
  - certificate revoked: every pairing of that device is cut (as today);
  - unpaired on the box (`ligus phones revoke`) or forgotten on the phone
    (**Forget host** now calls `DELETE /v1/tunnel/pairing` on the box first,
    best effort): the pairing is cut, the push token deleted, and the
    person's desktop gets an Inbox row, "Your phone 'Ana's iPhone' was
    unpaired from box-1. Remove it from your devices?", with **Remove** (a
    passkey). The box can't revoke a person's device, since it is a Service
    member, so the person confirms;
  - phone key reset: the new enrolment request names the old device id
    (`previousDeviceId`, inside the request, not on the log), and the desktop
    that certifies the new key offers to revoke the old one in the same
    ceremony.
- Revoked pairings are dropped from `phones.json` 30 days after revocation.
  The unused `ownerPhone` field is removed from the format.

### 10.3 Push

The audience is the principal (section 6). `PhoneRecord.push` holds
`{enabled, previews, kinds[]}` (kinds since 0.4.1). The phone sends its
preferences to **every** paired box on change and on foreground, so no box
keeps stale ones. The question cap is per (person, hour).

### 10.4 Member exits: `MemberLeft`

- `MemberLeft{memberId, accountId, watermark}` (new kind): a member leaves by
  itself. A person leaves from a non-delegated, non-phone device of their
  account, with a passkey when the account holds one. A box leaves with
  `ligus serve leave`, signed by its own device key. Never the last Owner or
  the founder.
- Under rules 3 it is display only, shown as **"asked to leave"** with Remove
  for Admins (the member still writes and still receives key rotations, so
  "left" would mislead). Under rules 4 (G4) it is a removal, judged like a
  self-removal.
- Two Owners leaving at once resolve in canonical order; the second is
  denied if it would leave no Owner ("Couldn't leave: you're the last
  Owner"). Between `MemberLeft` and `MemberRetired` for one member, the first
  applies and the second is denied.
- A box's leave makes its host inert and its agents `HostGone`.
- The leaver's watermark is chosen by the leaver, so it can name events it
  signed earlier and held back; those fold as pre-leave content. That is no
  more than the leaver could do by publishing them before leaving, so it is
  accepted. Rotations exclude the leaver's devices from the leave's position,
  and the next rotation after it is prompted.

## 11. The workspace layer

**In short:** config logs and chat logs get separate typed views, a chat can
no longer redefine a workspace agent, and the two kinds of workspace id
can't be mixed up.

- Two typed projections over the same fold: `WorkspaceConfig` (config log)
  and `ChatState` (chat log), each exposing only its own fields. Lockdowns and
  dearth reach a chat through a function of `(ChatState, WorkspaceConfig at
  the reader's frontier)` that every reader uses, not a side effect of
  `ChatService::load`.
- Writers stop authoring whole-list events (`AgentRosterUpdated`,
  `WorkspaceRuntimesUpdated`, `WorkspaceHostsUpdated`) and chat-level agent
  records (class L in 0.5).
- Today a chat-level agent record shadows the workspace agent of the same id
  inside that chat (`core/authz/mod.rs`). Under rules 4 (G5), agent records
  on a chat log are denied. Copies written before the rules-4 position keep
  folding; dispatch on 0.5 reads the workspace record and shows Admins "This
  chat has an old copy of @x" with **Use the workspace's @x**, which writes
  nothing.
- Under rules 4 (G11), whole-list agent, runtime and host events are denied
  on the config log.
- Code gets `WorkspaceId(Uuid)` and `RoomName(String)` newtypes, so the room
  string (a relay address only) and the workspace id can't be swapped. No
  wire change.

## 12. Lifecycles

**In short:** every row below has an exit, and a reference to something that
left renders as a named state.

| Entity | Created by | Renamed by | Retired or left | Revoked or removed | A gone reference renders as |
|---|---|---|---|---|---|
| Workspace | founder's seed snapshot and `MemberAdded` | `SessionTitleChanged` on the config log (Admin) | — | local Leave (deletes the connection) | — |
| Member (person) | `MemberAdded` through an invite | `ProfileUpdated` | `MemberRetired` (link duplicate), **`MemberLeft`** | `MemberRemoved` (Admin, passkey) | name from the profile + "(former member)" |
| Member (box) | admission and `MemberAdded` | `DeviceLabelSet` by the box | **`MemberLeft`** (`ligus serve leave`) | `MemberRemoved` | "box-1 (removed box)" |
| Device | `DeviceCertificateAdded` and acceptance | `DeviceLabelSet` | Idle (a view) | `DeviceRevoked` (passkey) | "Revoked device" |
| Host | `WorkspaceHostUpserted` signed by that device | label from `DeviceLabelSet` | inert when its owner is gone, its device revoked or its box left | `WorkspaceHostRemoved` (Clean up) | agents `HostGone`, never moved elsewhere |
| Source (offer) | the host record's `offers` | the host's operator | dropped from the next host record | host removed | `Unrunnable(SourceNotOffered)` |
| Phone pairing | tunnel enrolment on the box (with `deviceId`) | through `DeviceLabelSet` | — | unpair (box or phone), certificate revoke; purged after 30 days | "Unpaired phone" in My devices |
| Agent | one `AgentUpserted` with run and placement | `AgentUpserted` (owner) | — | `AgentRemoved` (passkey) | "@x (removed)"; mentions don't route |
| Runtime (legacy) | `WorkspaceRuntimeUpserted` | the same | not created by new UI | `WorkspaceRuntimeRemoved` | `Unrunnable(SourceNotOffered)` for agents naming it |
| Chat | seed snapshot | `SessionTitleChanged` | `SessionArchivedChanged` | (no delete on the log) | "Archived chat" |
| Message | `MessageAppended` | — | Posted, Failed, Canceled through `MessageCompleted{outcome}` | `MessageRemoved` | "Removed message" |
| Turn | derived from the claim and the placeholder | — | Posted, Failed, Canceled, Superseded | — | — |
| Proposal | `ProposalUpserted` | refinements | `ProposalDismissed` (archive), settled | `ProposalReverted` | — |
| Ask | `FlowRecorded{HumanAsk}` | — | settled, closed, expired (a view), restart sweep | — | "This question was closed" |
| Scratch repo | host-local | host-local | kept, notice, cleanup after 14 days | removed | "Scratch repo removed from <host>" |
| Attachment | `AttachmentShared` | — | relay expiry (renewed while live) | `AttachmentDeleted` | "This file expired on the relay" |

## 13. Local storage

**In short:** one additive database step in 0.5, the destructive ones in
0.6, and the box's files keyed by person.

| `user_version` | Release | Adds | Notes |
|---|---|---|---|
| 2 | 0.5 | `pruned_events`, `dispatch_processed`, `scratch_repos` and `scratch_repo_chats`, `workspace_folders` (v2 10.2), `principal_pins` (the single-holder pin for a phone, typed) | additive tables and imports, nothing destructive |
| 3 | 0.6 | the events `place` split (v2's M5) | table rebuild with a `VACUUM INTO` backup |
| 4 | 0.6 | deletes the 0.3-era sync tables, legacy secret ids and the dormant local account seed | only after a release that reads both |

Each step follows `migrate_event_ids_per_workspace`: probe, back up, one
transaction, remove a partial backup on failure. A build that opens a higher
`user_version` opens read-only (since 0.4).

Files:

- v2's one connection list (`workspaces.json`, 10.1) and one code-folder
  binding table (10.2), deferred from 0.4, land with `user_version` 2.
- `serve-state.json` v2 keys read marks by person; `phones.json` v2 requires
  `deviceId` on new records.
- `host.toml` (new, never synced) holds a host's sources: CLIs it detected,
  local servers with their URLs, provider secret references per workspace,
  and which agents may run there. The gateway list stays in its own file.
- lt's `lt-state.json` `read` becomes `{workspace: {chat: ms}}`; its
  `device-label` follows synced renames.
- On the phone, `HostProfile` and `PhoneBinding` stay caches of `/v1/me` with
  a `fetchedAt`; preferences are sent to every host.

Read marks, Hide for me, the per-chat model choice and notification
preferences belong to a **person**, but in 0.5 they stay on each device.
Putting them on a workspace log would show them to every member. In 0.6 the
person's private Personal workspace carries a `personal-state` log of small
last-writer-wins records, sealed to the person's own devices.

## 14. The relay

**In short:** one additive route for short codes; nothing else new.

- One additive route maps a 6-character short code to a person's approval
  address (section 17.2). The approval address is public and grants nothing,
  so a short code for it is safe. Existing `/v1` routes stay byte-compatible.
- Approval mailboxes stay in memory by design: a relay restart aborts
  in-flight approvals, and requesters post again (17.2).
- No new presence slots: host beats keep one slot per device. Presence has no
  delete route or expiry and a bounded number of slots per room, so any later
  use of presence for live text would first need those on the relay.
- Blob delete authority on gated relays keeps using the relay roster, an
  operator feature; Ligus clients never rely on it.

## 15. Rules version 4

**In short:** rules 4 is one switch that turns on everything in this note
that changes what the fold allows. New workspaces start on it; an Owner
switches an existing one from a readiness list. Older builds go read-only on
a rules-4 workspace.

### 15.1 What it adds

| # | Rule | Section |
|---|---|---|
| G1 | Agent placement and run authz (who may create and move agents) | 5.7 |
| G2 | Placement references must resolve: the host exists, isn't revoked, its owner is a member, it offers the source | 5.7 |
| G3 | Hosts of revoked devices and departed boxes are inert | 5.7 |
| G4 | `MemberLeft` is a removal | 10.4 |
| G5 | No agent records on chat logs | 11 |
| G6 | The default agent's well-known id | 5.8 |
| G7 | A Human or Service `MemberAdded` needs an `account_id` (no new accountless members; existing ones keep working and are listed for Admins) | — |
| G8 | A payload "who" hint that disagrees with the envelope's stamp is denied | — |
| G9 | A `WorkspaceRuntime` upsert that adds or changes a non-empty `endpoint` is denied; one that keeps it unchanged is allowed | 5.5 |
| G10 | Question eviction by canonical position | 9 |
| G11 | Whole-list agent, runtime and host events denied on the config log | 11 |
| G12 | `ChatChannelChanged` judged by workspace role | 9 |
| G13 | A host record counts only when signed by its own trusted device, owned by an account holding it | 5.1 |
| G14 | Agent keys accepted from every host the placement admits, and from a `TurnDelegated` device for that one turn | 5.3 |
| G15 | A phone names itself: `DeviceLabelSet` for its own device under its person, signed by the phone (`authz::profiles::PHONE_NAMES_ITSELF_FROM`); its person's desktop still renames it on every rules version | 17.6 |

Every rules-4 check on agents judges the merged record (5.6 rule 5). Events
before the rules-4 position are judged as before and never re-judged.

### 15.2 The switch

- **New workspaces** founded by 0.5 move to rules 4 with the founder's first
  credential, as rules 3 does in 0.4 (`adopt_founding_rules`).
- **Existing workspaces**: an Owner switches from Settings > Security, with a
  readiness list and one passkey. The list shows devices last seen on builds
  below 0.5 (from the build version in host and person beats), agents whose
  placement wouldn't resolve, host records not signed by their device, and
  accountless members. The list informs; the Owner can switch anyway.
- After the switch, 0.4 builds go read-only on that workspace with "update
  Ligus".

## 16. Migrations and mixed versions

**In short:** every 0.5 migration is additive and idempotent, and keeps the
old source readable. A 0.4 peer keeps working on a rules-3 workspace and
routes every agent the same way a 0.5 peer does.

### 16.1 Event and field additions

| Addition | Class | Ships |
|---|---|---|
| `WorkspaceAgent.run`, `.placement` (sticky) | R for display, L for dispatch, G1 and G2 for authz | 0.5 |
| `WorkspaceHost.offers` (sticky; `model_gateways` still written) | R and L | 0.5 |
| `MessageCompleted.outcome` | R | 0.5 |
| `TurnDelegated{triggerId, agentId, deviceId}` (chat log) | inert under rules 3, G14 under rules 4 | 0.5 |
| fold-only `host_gone` on agents | L (fold state, never written) | 0.5 |
| `MemberLeft` | R under rules 3 (display), G4 under rules 4 | 0.5 |
| enrolment request `previousDeviceId` (inside the code, not on the log) | L | 0.5 |
| the default agent's id and record | G6 | 0.5 (rules 4) |
| `personal-state` log in the Personal workspace | L (a new workspace, no new kind on shared logs) | 0.6 |

### 16.2 Local migrations

Numbered after v2's M1 to M7.

| # | When | What | Safety |
|---|---|---|---|
| M8 | 0.5 first start | `user_version` 2 tables (section 13) | additive; `ligus.db.lock` taken exclusive, skipped for this start if busy (the 0.4 pattern) |
| M9 | after M8 | import the scratch blob; import `workspace_dirs`, `repo-bindings.toml` and lt `checkouts` into `workspace_folders`; connections into `workspaces.json` | old sources kept read-only through 0.5 |
| M10 | after M8 | backfill `pruned_events` from witnesses; seed `dispatch_processed` with every stored message event that is answered or older than the age bound (recent unanswered ones stay eligible) | idempotent |
| M11 | serve 0.5 start | `serve-state.json` v2 (the box's one read-marks map is copied to every person with a paired phone) and `phones.json` v2 | old files kept as `.v1.bak` |
| M12 | box 0.5 start | `last_self_label` set to the current synced label | stops the restart revert |
| M13 | 0.5 desktop, once per workspace after a full config sync | **rules 3**: write `run = {source: "runtime:<id>", resolved: {harness, model}}` and the placement `host_id` implies, for agents this person owns; `runtime_id` untouched, so no Admin or passkey. **Rules 4**: offer "Pin @x to Claude Code / opus" per agent, which writes the full run spec (one confirmation per batch of moves). Two of the owner's devices racing write the same `source`; `resolved` is a hint | only the person's own agents; Admins get a list of the others |
| M14 | 0.5 hosts, on start | publish `offers` | names only |
| M5 | 0.6 | events `place` split (v2) | v2's M5 |
| M15 | 0.6 | delete legacy sources | after a release that reads both |

A 0.4 `ligus serve` already running beside a 0.5 CLI keeps working until it
restarts (M8 only adds tables). After a restart it opens the database
read-only. Release notes and `ligus doctor` say to restart the box with the
CLI, and `ligus-local upgrade` does both.

### 16.3 Mixed versions

| Situation | 0.4 peer | 0.5 peer |
|---|---|---|
| Agent with `run` and `placement`, rules 3 | uses `runtime_id` (unchanged) and `host_id` (always consistent) | uses `run` (a mirror) and the placement; the same routing |
| A 0.4 worker meets a full run spec | only possible in a rules-4 workspace, where it is read-only; 0.5 clients also never place full specs on hosts below 0.5 | — |
| A 0.4 peer renames an agent | writes the whole record without sticky fields | keeps `run` and `placement` (5.6 rule 1) |
| A 0.4 peer moves an agent (`host_id`) | as today | legacy wins, the placement follows (5.6 rule 2) |
| Host `offers` | ignored; `model_gateways` read as today | used |
| `MessageCompleted.outcome` | ignored | shown |
| Coalesced chunks from a 0.5 writer | streams in larger steps | the same |
| `TurnDelegated`, rules 3 | no-op | inert |
| `MemberLeft`, rules 3 | no-op, the member stays | "asked to leave" with Remove for Admins; authz identical |
| Rules 4 set | read-only, "update Ligus" | applies G1 to G14 |
| A 0.4 desktop and an agent placed on a worker | `host_id` is the worker, so it doesn't run it (correct) | — |
| A 0.4 desktop and an `ownerDevices` agent | resolves `runtime_id` in its catalog, with its fallback (today's behaviour) | resolves the same `runtime:<id>`; the claim arbitrates if both try |
| A 0.4 box and a 0.5 phone | no principal fixes; 0.4.1 behaviour | — |
| A 0.5 database opened by 0.4 | read-only with the newer-data message | — |

## 17. The onboarding model this enables

**In short:** one person is one member by construction, a new device links
by typing a short code, terminals ask the person's desktop or phone for the
passkey, an agent is one step, and one checklist shows what is set up. The
strict defaults stay, each with one line that says why and what to do.

### 17.1 The first run asks one question first

Every surface asks, before a name: **"Is this your first device with
Ligus?"** (Yes, start here / I already use Ligus on another device).

- **First device**: You (name prefilled from git, optional picture), then
  Secure (passkey and recovery key, one step, one button; skippable only as
  "Just me on this computer for now", which leaves "Passkey · Not set up" in
  the checklist), then the first agent (the one-step sheet, prefilled with
  the best detected source), then the chat. The first workspace is made for
  the person. Relay, team and folder are asked when they first mean
  something (inviting, adding a phone, picking a folder). Theme moves to
  Settings.
- **Another device**: link (17.2). The new device asks for no name; the
  profile comes from `ProfileUpdated`, and the device name is chosen from the
  OS once and editable in My devices. No new member is ever made.
- **Invite guard**: when an invite is opened on an install that is already
  linked anywhere, the redeem screen says "Already use Ligus? Link this
  device instead". It is a hint, not a refusal. Invites stay for other
  people.

### 17.2 Link a device with a short code

1. The existing desktop's **Add a computer** shows a 6-character code that
   the relay resolves to the person's approval address.
2. The new device (desktop, lt, or `ligus devices request --code K7P2QX
   [--relay URL] [--wait 10m] [--json]`) types it. That posts the link
   request to the mailbox. (Shipped: `ligus devices link` is the showing
   side in a terminal; the derivation is in `code-match-wire.md` section 9.)
3. Both screens show the match code. **Codes match** on the approving desktop
   or phone (passkey), the grant is sealed back, accepted and named. Nothing
   is copied or pasted.
4. The CLI exits 4 while waiting, 0 when linked, 7 if the relay can't be
   reached.
5. Fallback, behind (i): "Can't reach the other device? Use a request code"
   (today's paste path).

This needs the desktop to request and redeem links itself (the runtime has
it; the app doesn't expose it yet), the relay short-code route (section 14),
and the identity bootstrap for "another device" deferred until the link
completes (v2's `Binding::Linked`). If the relay restarts mid-link, the
requester posts again and both sides say "The relay restarted; asking
again".

### 17.3 Passkey hand-off from terminals

The CLI and lt can't hold a passkey. A new approval-mailbox subject,
`Presence`, carries a presence request from a terminal to the same person's
desktop or phone:

- The terminal drafts the event exactly as it will publish it, builds the
  `PresenceRequest` it already builds for the browser hand-off (challenge,
  kind, account, credential ids, short digest), and posts it, sealed and
  signed by its device key, to the person's own approval address.
- The desktop (an Inbox row and a notification) and the phone (a push, then
  the same sheet) show "**build-1** asks to: Add agent @coder (claude-code,
  sonnet), code **4F7K**" with Confirm with passkey / Not me.
- The approver checks that the requesting device holds the account and isn't
  revoked, and renders the summary **from the draft itself**, never from text
  the requester supplied. It runs the ceremony over the challenge and seals
  the assertion back.
- The terminal prints "Confirm on your desktop or phone: Add @coder (code
  4F7K)" and waits (a spinner in lt; `--wait` and exit 4 with a `next:` line
  in the CLI), then publishes. The fold checks presence exactly as today.
- "Not me" revokes nothing by itself; it raises an Inbox row offering to
  revoke that device. Requests are rate-limited per device.

No rule, wire id or fold changes: the passkey still signs
`presence::challenge(envelope)`, which covers the workspace, log, event id,
lamport, payload, frontier and stamped account. The phone can approve in 0.5.

### 17.4 Agents in one step

The sheet ("Add an agent", `/setup agent`, the command palette, the Tools
pane, the first run) is one setup-engine form:

1. **Name** (not reserved, no clash).
2. **What answers**: one list built from the offers of every host the
   person can place an agent on, each with its state from the beat: "Claude
   Code · signed in · this Mac", "codex · signed in · build-1", "gw-home
   (OpenAI API) · offered by box-1", "Ollama · this Mac", "Anthropic API ·
   key by secret reference on box-1".
3. **Model**: that source's list from the beat, with free entry where the
   source allows it.
4. **Runs on**, defaulted: a CLI harness on the device where it is signed in
   (this device first); a gateway on the host that offers it; a key on this
   device here; a secret reference on the hosts that declare it. One line
   below: "Turns run on build-1. When it's offline, @coder waits."
5. **Sandbox**, when relevant (for agy, "May run local servers", Owner only,
   off by default).
6. **Test turn**, on by default for the first agent.
7. One button, **Add @coder**: one event and at most one passkey confirmation
   on rules 4 (two, with a step count, on rules 3 when a workspace runtime is
   needed, 5.9).

Afterwards the agent's ⋯ menu has **Change model…** and **Runs on…**,
through the same form. The CLI's `ligus setup agent` and lt's form are the
same engine form, and use the hand-off (17.3) for the passkey.

### 17.5 One checklist: "Set up Ligus"

One row per item, each with a state and at most one action; details behind
(i). Desktop: Settings > Set up (also the empty Home and the command
palette). CLI: `ligus setup`. lt: its setup screen. Phone: Health.

```
✓ You            Ana · picture
✓ Passkey        2 passkeys · recovery key
✓ Workspace      Team · rules 4 · relay relay.example
• My devices     this Mac, build-1 (lt) · 1 approved, never used      [Review]
○ Phone          Not set up                                           [Add a phone]
✓ Box            box-1 · 0.5.0                                        [How to upgrade]
✓ Agents         @coder on build-1 · @ligus on this Mac
○ Folder         Not set up                                           [Add a folder]
• Clean up       1 former member's host · 1 device never finished     [Clean up]
```

- Grey "Not set up" for things never chosen, amber for chosen but degraded,
  red only for chosen and failing.
- **Clean up** gathers the leftovers: hosts of former members, devices never
  finished, retired or "asked to leave" members, unpaired phones. Each removal
  is still its own passkey ceremony (no silent revokes), run in one sheet with
  "Confirm 1 of 3". One passkey over a batch is 0.6 (D17).
- The rows come from the setup engine's status plus a few facts (passkeys,
  rules version, cleanup counts), so every surface shows the same thing.
  Status stays local and cheap; box and phone liveness come from beats.

### 17.6 The phone in one flow

1. Scan the code (`ligus pair` on the box, or the desktop's Add a phone).
2. **Name this phone**, prefilled ("Ana's iPhone").
3. **"Add this phone to Ana's devices in Team?"**: one confirmation that also
   signs the enrolment statement inside the request. The separate "this phone
   is Ana's" alert remains only for phones enrolled before 0.4. The certifying
   desktop writes the phone's label in the same commit (10.1).
4. **Notifications**, asked once on builds that can push; on builds that
   can't, Health shows grey "Not available in this build".
5. Done: Health shows the checklist rows.

The phone can rename itself (v2's O3: "the device itself").

### 17.7 Boxes and gateways are commands

- `ligus gateway add NAME URL --api openai|anthropic [--auth …]
  [--allow-plaintext] [--not-exclusive]`, `list`, `check`, `remove`. It
  writes the allow-list atomically to a default file in the data dir that the
  box reads when `LIGUS_MODEL_GATEWAYS_FILE` is unset.
- `ligus secrets set NAME -` (reads stdin, writes an owner-only file), `list`
  (names only), `rm`.
- `ligus setup box --to <approval address>` (and the matching `ligus-local`
  option) admits a box by code match, with nothing pasted.
- `ligus-local upgrade` upgrades, restarts the unit, verifies and reports the
  version; `ligus-local cleanup --legacy` lists and removes leftover hive-era
  units, LaunchAgents and data dirs after showing them. The formula gets a
  `service` block. The desktop host row and the phone's Health show "box-1 is
  on 0.4.1; 0.5.0 is out · ligus-local upgrade" from the build version in the
  beat.

### 17.8 Strict defaults, one line each

| Default (unchanged) | The line at the point of refusal |
|---|---|
| Passkey for agents, runtimes, devices, members, repos | "Confirm with your passkey: it proves this change is yours." (terminal: "…on your desktop or phone") |
| No passkey yet | "Add a passkey to change who's in and what agents may do." [Add a passkey] |
| Gateway-exclusive once a gateway is listed | "This box sends model traffic only through its gateways." [Allow direct traffic too] |
| Plain http refused off loopback | "Plain http only works on this machine. Use https://." |
| Gateway http needs the operator's `allowPlaintext` | "Plain http to a gateway is the box operator's choice: ligus gateway add … --allow-plaintext." |
| Sandbox never offered off | "Agents run only inside the sandbox. This host has none: see ligus doctor --sandbox." |
| Keys never in the workspace | "Keys stay on the device that uses them: ligus secrets set NAME - on that host." |
| No automatic revokes | "Removing a device needs your passkey." |
| A box is its own member | "Boxes join as their own member. Admit it with a passkey." |

## 18. Invariants and their tests

**In short:** each claim in this note has an executable test that has to pass
before the change it covers merges. Property tests run over permutations of
the same event set unless stated.

### 18.1 Log and fold

| # | Invariant | Test |
|---|---|---|
| L1 | The fold is a function of the event set: any arrival order gives the same projection, including sticky fields and the legacy-wins rule. | `ligus-core` proptest: agent upserts from old (no sticky fields) and new writers, permuted |
| L2 | A device's next `prev` is its newest event by `(lamport, event_id)`, whatever the row order. | `ligus-runtime`: prune, re-pull out of order, post 20 times from two devices of one person; no stale draft |
| L3 | A pruned event fetched again is never stored as a row again. | `event_store`: prune, re-ingest the same envelope; `events` count unchanged, one witness |
| L4 | No reader orders by `row_id` except an allow-list. | source lint over the `rt/event_store*.rs` SQL |
| L5 | Dispatch answers every message once: one written offline below the chat's head is answered; one already processed, tombstoned or older than the age bound (by envelope time) is not. | `turn_dispatch`: offline laptop writes at head 50, chat reaches 80, laptop syncs: answered once; a cursor reset re-pulls a 3-day-old message: no claim |
| L6 | Under rules 3 every new kind and field is inert for authz, and verdicts match a pinned 0.4 core. | a `compat/v04` crate (the `compat/v03` pattern) folding generated logs with every 0.5 addition interleaved |
| L7 | With rules 4 set, a 0.4 build reports read-only and a 0.5 build applies G1 to G14; events before the rules-4 position are judged as before. | the same crate |
| L8 | A pinned 0.4 `ligus-runtime` ingests, stores and re-pushes every 0.5 event byte for byte; none becomes a witness or is held. | `crates/ligus-runtime/tests/mixed_version.rs` |

### 18.2 Workspace and identity

| # | Invariant | Test |
|---|---|---|
| W1 | Under rules 4 a Human or Service `MemberAdded` without `account_id` is denied; existing ones keep folding. | `ligus-core` |
| W2 | Under rules 4 no agent record is accepted on a chat log; earlier chat copies aren't re-judged. | `ligus-core` |
| W3 | `MemberLeft` never removes the last Owner or the founder; two Owners leaving at once leave one; a delegated or phone device can't write it for a person; under rules 3 authz output and the rotation set are identical with and without it; of a leave and a retire for one member, the first applies. | `ligus-core` |
| W4 | `WorkspaceId` and `RoomName` can't be passed for each other. | compile-fail test (`trybuild`) |

### 18.3 Agents, hosts and sources

| # | Invariant | Test |
|---|---|---|
| H1 | An agent's model is the same on every device that runs it: dispatch resolves `run` through the running host's offers, never a device catalog id. | `ligus-runtime`: one agent run on a desktop and a worker fixture report the same `outcome.model` |
| H2 | No silent substitution: an unknown source, missing key or a model absent from a complete list gives `Unrunnable(why)` and writes no placeholder. | `turn_dispatch` table test over every `Why` |
| H3 | An agent placed on an offline host never runs elsewhere without an explicit "Run here this time", and the sender sees `Queued`. | `app` test: stale host beat, send; no claim from the sender's device, a queued row |
| H4 | Creating an agent is one event on every surface (desktop, CLI, lt, `ligus setup agent`), with at most one passkey ceremony on rules 4. | runtime test counting events and ceremonies per surface |
| H5 | Under rules 4, placing an agent on a host that doesn't exist at that position, is revoked, has a removed owner, or doesn't offer the source is denied. | `ligus-core` |
| H6 | A gateway source's harness comes from the offer's wire; a member can't make an `anthropic`-wire gateway run as OpenAI. | `model_gateway` tests |
| H7 | No URL, secret or account string appears in a host record, offer, agent record or beat, and a host resolves only secret names its `host.toml` declares for this workspace. | fuzz the host config with URLs and keys; scan written payloads for `://`, userinfo and key prefixes; an agent naming an undeclared secret is `Unrunnable(NoKeyHere)` |
| H8 | An older client's agent upsert never erases `run` or `placement`; one that changes `host_id` moves the agent on every build. | `ligus-core` with a pinned 0.4 `WorkspaceAgent` serializer |
| H9 | A desktop runs only its own person's agents unless its person opts in. | `app` dispatch test |
| H10 | Authz judges the merged record: under rules 4 a Contributor's 0.4-shaped upsert with no placement and `host_id` set to someone else's host is denied. | `ligus-core` proptest over payload shapes |
| H11 | A host record under a device id that didn't sign it is ignored by pickers and workers (0.5) and denied under rules 4; it never satisfies G1 or G2. | `ligus-core` and worker test |
| H12 | Under rules 4 a reply from a non-first `pool` host and one under a `TurnDelegated` are accepted; a reply from any other device's agent key is denied. | `ligus-core` |
| H13 | The agents of a removed, revoked or departed box are `HostGone` and never run on the owner's desktop. | `ligus-core` and `app` dispatch test |
| H14 | `legacy_id` round-trips for every model name (`llama3:8b`, `org/model`, unicode), and `runtime:<id>` maps to `<id>`. | `ligus-core` proptest |
| H15 | On a rules-3 workspace M13 writes only `run.source = runtime:<id>`, the fold keeps it, and `runtime_id` is never rewritten. | `ligus-core` and desktop migration test |
| H16 | A 0.5 client refuses to place a full run spec on a host whose beat reports a build below 0.5; a 0.5 worker refuses a full spec it can't resolve rather than using `*`. | `deploy/tests` with a pinned 0.4 binary; worker test |

### 18.4 Principals, phones and push

| # | Invariant | Test |
|---|---|---|
| U1 | No "you"-dependent serve path reads the box's own identity. | source lint over `cli/serve` (`my_actor_id`, `svc.author()` only in an allow-list of writers) |
| U2 | A notice for a person reaches only that person's phones; an unresolved phone gets no "you" notices and no previews. | `serve::tests::tunnel_e2e` with three phones (Ana, Ben, unresolved) |
| U3 | Unread for a person never counts messages they wrote on any device. | serve API test: a phone posts, then lists chats |
| U4 | Unpair on the box or Forget on the phone deletes the push token and raises the desktop row; a certificate revoke cuts every pairing. | tunnel e2e and a phone unit test for the Forget call order |
| U5 | Every phone gets a synced label at enrolment; "Device xxxxxxxx" never appears. | `app` and `phone_sign` tests |
| U6 | A desktop rename of a box survives the box's restart; a new `--label` changes it. | serve test |
| U7 | Push kinds the phone turned off are not sent. | serve push filter test |
| U8 | A phone lists no DM it isn't part of, can't write as the box, and a direct-path request with only the shared bearer is `Unresolved`. | serve API tests |

### 18.5 Content

| # | Invariant | Test |
|---|---|---|
| E1 | A reply's display time is its completion's envelope time and is never earlier than its trigger's. | `ligus-core` fold test (extends #275) |
| E2 | A failed or canceled turn renders as such; the stale sweep writes `canceled`. | runtime test |
| E3 | A streamed reply writes at most one chunk per 2 s or 4 KiB; the secret hold-back still applies; phones still get stream deltas; a crash sweep keeps the text so far. | runtime test (port the split-secret test) and a serve stream test |
| E4 | Under rules 4 question eviction is by canonical position: a far-future `at_ms` doesn't keep a question resident. | `core/flow/asks.rs` tests |
| E5 | The scratch registry never shrinks because of a parse error. | corrupt the old blob: the import stops, rows are kept, doctor warns |
| E6 | `Turn.state` is a pure function of the chat fold (plus the local route). | proptest over claim, answer and remove permutations |
| E7 | A `canceled` from a stale sweep is overridden by a later completion from the placeholder's signer. | `ligus-core` |

### 18.6 Storage

| # | Invariant | Test |
|---|---|---|
| V1 | Every `user_version` step is all-or-nothing, backed up and idempotent, and a 0.4 binary opens a version-2 database read-only with the message. | `event_store` migration tests and `deploy/tests` with the pinned 0.4 binary |
| V2 | A 0.4 `ligus serve` already running beside a 0.5 CLI keeps working until it restarts. | `deploy/tests` |
| V3 | No secret appears in any new table or file. | byte scan, as v2's M2t |

### 18.7 Onboarding

| # | Invariant | Test |
|---|---|---|
| S1 | Choosing "I already use Ligus" never creates a member; the new device ends linked to the existing person or not at all. | desktop and lt onboarding tests |
| S2 | A short code resolves only to an approval address and carries nothing secret; an unknown or expired code is a clear error. | relay route tests; client tests |
| S3 | A presence hand-off summary is rendered from the draft, never from requester text; a request from a device that doesn't hold the account, or is revoked, is refused; an assertion is valid only for the exact draft. | `approval_mailbox` tests with a tampered summary and a swapped draft |
| S4 | The checklist shows the same rows on the desktop, `ligus setup`, lt and the phone for the same state. | shared fixture rendered by each surface |

## 19. Owner decisions

All decided on 2026-11-26, as recommended.

| # | Question | Decision |
|---|---|---|
| D0 | The defects in today's code found while writing this note | **Fix them with small, local changes, cross-person exposure first.** 0.4.1 fixed the DM listing, phone notes, the catch-all notice, the scratch registry and notification kinds (1.7); the rest is in 0.5 |
| D1 | What an agent names | **Harness, source and model directly, on rules 4**; workspace runtimes stay readable as legacy presets; on rules 3 the agent sheet reuses runtimes underneath. New UI creates no visible runtime records |
| D2 | Default placement of a new agent | **"My devices"** for a personal agent; a specific host when the person picks an always-on one |
| D3 | When the bound host is offline, or gone | **Queued (offline) or HostGone (removed), visibly, in the chat**, with "Run here this time" for the agent's owner when this device has the same source (a signed one-turn delegation under rules 4). The silent answer-locally fallback and the silent Claude Code substitution are removed |
| D4 | What a host advertises | **Source names and kinds on the host record** (harnesses, gateways, declared secret-reference names; never URLs or values); **readiness, model lists and build version in the sealed beat**. A person's own computers list by default with an opt-out; workers and boxes always |
| D5 | Model server addresses on the log | **Move them to the host** as offers; existing ones keep working with a notice; rules 4 refuses new ones |
| D6 | Streaming chunks | **Keep them on the log, coalesced (2 s or 4 KiB), pruned at most daily behind tombstones.** Live text over relay presence is later and needs relay expiry, delete and signed frames first |
| D7 | Where a finished reply sits in the transcript | **Where it finished**, with a live row at the end while it streams |
| D8 | A phone the box can't place | **Fail closed**: workspace-wide notices only, no previews, no "you" notices |
| D9 | Pairing and device lifecycles | **Two-way**: unpair on the box or Forget on the phone deletes the push token and asks the person to remove the device; a key reset offers to revoke the old key in the same ceremony; revoked pairings purged after 30 days |
| D10 | A member leaving by itself | **`MemberLeft`**, including `ligus serve leave`; a removal under rules 4 |
| D11 | Rules 4 | **New workspaces start on rules 4 in 0.5; existing ones switch by an Owner with a readiness list** |
| D12 | Accountless members | **Refused under rules 4**; existing ones listed for Admins |
| D13 | Read marks, Hide for me, model choice and push preferences across a person's devices | **Device-local in 0.5; a sealed personal-state log in the person's Personal workspace in 0.6** |
| D14 | Periodic snapshots | **Stop writing non-seed snapshots** |
| D15 | The default agent | **A real record with a well-known id under rules 4**, configurable like any agent |
| D16 | Who may run on a desktop host | **Only its own person's agents**, unless the person opts in; workers and boxes run any agent placed on them |
| D17 | Several passkey ceremonies in one step (Clean up, Approve & apply) | **One passkey over a batch digest, in 0.6.** 0.5 runs them in one sheet with a step count; with rules 4's one-event agent the agent sheet needs only one anyway |
| D18 | Short codes for approval addresses | **A new additive relay route**; existing `/v1` routes unchanged |
| D19 | One ceremony for "runtime + agent" on rules 3 | **Two, with a step count, in 0.5**; batching is D17 |
| D20 | Passkey hand-off approvals from the phone | **Yes, in 0.5**, beside the desktop |
| D21 | Hosts advertise harness sign-in state and held secret-reference names | **Yes** (names and states only, in the beat; D4) |
| D22 | Invite guard strength | **A hint only** ("Link this device instead"); self-invites aren't refused |

Owner guidance that applies throughout: fix what is likely to happen before
what is merely possible, and prefer a better flow over a patch; keep the UI
calm; keep the security defaults strict; never rename a wire id; model keys
never go on the log.

## 20. Threats and residual risks

| # | Threat | Defence | Residual |
|---|---|---|---|
| T1 | Offers tell every member, Viewers included, which tools a colleague's computers have | Offers name harnesses and gateways only, never accounts, plans or URLs; readiness is in the sealed beat; an opt-out for personal computers | Members learn which harnesses each other's computers offer |
| T2 | An older client's whole-record upsert forks routing between 0.4 and 0.5 devices | Sticky fields with legacy-wins (5.6); tests L1, H8 | None while the rule holds |
| T3 | A Contributor moves an agent to someone else's host with a 0.4-shaped upsert | Authz judges the merged record; any effective change is a move (5.6 rule 5); test H10 | None |
| T4 | A Contributor registers a host under another person's device id to make it "theirs" | Readers ignore such records in 0.5; G13 denies them; test H11 | None after rules 4 |
| T5 | A provider offer is used to read another workspace's secret on a shared worker | Hosts resolve only names `host.toml` declares for this workspace; test H7 | None |
| T6 | A removed box's agents quietly run on the owner's desktop with the owner's credentials | `HostGone` is a state, never a fallback; test H13 | None |
| T7 | "Run here this time" or a pool host's reply is denied, or any device's key is accepted | G14 and the one-turn `TurnDelegated`; test H12 | None |
| T8 | A re-fetched old message is answered again, or an offline message is never answered | Processed set by event id, tombstones, envelope-time age bound; test L5 | None |
| T9 | A box hands its own notices and previews to a phone whose person is unknown | Fail closed (section 6); test U2 | Older boxes keep 0.4.1 behaviour until updated |
| T10 | A phone writes as the box, or sees another person's DMs | Phone-signed actions or refusal; listing by principal; per-phone direct-path auth; test U8 | None |
| T11 | A stolen desktop makes its person leave every workspace | `MemberLeft` needs a passkey when the account holds one; never from linked computers or phones; never the last Owner or founder; test W3 | None beyond what the thief could already do as that member |
| T12 | A box revokes a person's phone on its own | The box only cuts transport and raises the person's desktop row; the person revokes | None |
| T13 | A forged `canceled` or `outcome` misreports a turn | Display only, labelled "reported by <host>"; a later completion from the placeholder's signer wins; spend accounting stays on the host | An agent key can claim a different model in its outcome |
| T14 | A terminal gets a passkey signature over something the person didn't see | The approver renders the summary from the draft and signs the challenge over that exact envelope; only the person's own devices can ask; rate limits; test S3 | None |
| T15 | A model change on a stolen linked device drives spend | Only within the same source; any source or harness change needs a passkey; the agent's limits apply | Model changes in one source need no passkey on rules 4 |
| T16 | Tombstones hide an equivocation | Tombstones key by envelope digest; a different copy goes to `equivocations` | None |
| T17 | Two builds fold the same log differently | Class G only behind rules 4, where older builds are read-only; class R never feeds authz; tests L6 to L8 | None while the class rules hold |

Other residual risks, accepted:

- A leaver's own watermark can carry events it signed earlier and held back;
  they fold as pre-leave content (10.4).
- Until a workspace is on rules 4, per-agent models aren't available there,
  and a model change stays an Admin action.
- Copying the box's one read-marks map to every person (M11) marks some
  chats read for people who hadn't read them. It happens once.
- The person's cross-device state stays device-local until 0.6.
- Older phones and boxes keep their behaviour until they update.
- Approval mailboxes don't survive a relay restart; requesters post again.

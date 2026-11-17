<div align="center">

<img src="assets/branding/hive-app-icon-1024.png" alt="Ligus" width="112" height="112">

# Ligus

**A collaborative, multi-agent workspace for developers.**

Ligus (LEE-gus) is short for *ligustica*, as in *Apis mellifera ligustica*,
the Italian honeybee. It was called Hive until v2.0.

Chat with one or many coding agents, review their proposed changes, and sync the
whole workspace across your devices and teammates — peer-to-peer or through a
relay, end-to-end encrypted. Your code and keys stay on your machine.

[![CI](https://github.com/ligus-ai/ligus/actions/workflows/ci.yml/badge.svg)](https://github.com/ligus-ai/ligus/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ligus-ai/ligus?include_prereleases&sort=semver&label=release)](https://github.com/ligus-ai/ligus/releases/latest)
[![Docs](https://img.shields.io/badge/docs-docs.ligus.ai-4c8bf5)](https://docs.ligus.ai/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)

[**Download**](#install) · [**Documentation**](https://docs.ligus.ai/) · [**Build from source**](#build-from-source) · [**Contributing**](CONTRIBUTING.md)

</div>

> **Hive is now Ligus**, short for *ligustica*. Same project, same protocol,
> new name. The command is `ligus` and settings are `LIGUS_*`. Ligus 2.0
> starts fresh: Hive 1.x data is not migrated and stays where it is,
> untouched, so 1.x keeps running until you remove it. See
> [Upgrading from Hive 1.x](https://docs.ligus.ai/deploy/upgrade-from-hive/).

---

## Overview

Ligus is a small native desktop app built with **Rust + [Tauri v2](https://tauri.app) +
React/TypeScript**, so one codebase ships on **macOS, Linux, and Windows**. It
turns a coding session into a room: you, one or many AI coding agents, and — when
you want them — your teammates, all sharing a single transcript and a single
working tree.

It's **local-first**. Agents run against the working tree on your own machine,
your model keys never leave the device, and the whole workspace is an
append-only, signed **event log** stored in a local SQLite database. There's no
account you have to create and no server you're forced to trust to get started —
a workspace with no relay puts nothing on the wire at all.

When you *do* want to collaborate — across your own laptop and desktop, or with
other people — Ligus syncs that event log **end-to-end encrypted**. It can go
directly peer-to-peer ([iroh](https://iroh.computer)) or fall back to a **relay**,
and the relay is deliberately **content-blind**: it stores and forwards sealed
ciphertext and never holds the key, so it can't read your traffic. That relay is
a single small binary you can **self-host** yourself.

That last part is the wedge. Plenty of tools let a team share AI coding sessions
— but they tend to charge per seat for the cloud sync and team features, and the
sync runs through *their* servers. Ligus's collaboration layer is **free,
open-source, and self-hostable**, and it's built so the party that relays your
traffic mathematically can't see it. You keep your code, your keys, and your
history; the network only ever sees noise.

**Who it's for:** developers who want to drive coding agents from a real desktop
app instead of a terminal; small teams who want to review agent changes together
without handing their source to a SaaS; and anyone who wants an always-on agent
that keeps working while their laptop is closed — on infrastructure they control.

<div align="center">
<img src="docs-site/images/overview.png" alt="Ligus — a shared LLM workspace" width="820">
</div>

## Features

### Multi-agent chat & channels

A Ligus chat is multi-party from the start: the human(s), the chat's **Primary
Runtime**, and any number of **workspace agents** all post into one transcript.
You route turns with `@mentions` — `@agent-name`, `@primary`, a teammate's
handle, or a governance-role group like `@admins` — and agents can mention each
other, so a blocked sub-agent can escalate to a human or another model
(fan-out is depth-capped to prevent runaway loops). **Channels** organize the
work, and long threads are automatically condensed to fit the model's context
window. You can **Stop** a streaming turn and send a new message at any time, and
concurrent tasks to the *same* agent — from different channels or teammates —
stay distinct, so several people can drive the shared agents at once.

### Bring your own agents & runtimes

Model lists come from each runtime you connect, not a hardcoded set. Out of the
box Ligus drives:

- **The `claude` CLI** — no API key required, uses your existing Claude auth.
- **Anthropic**, **OpenAI**, **OpenRouter**, **Ollama** (local or over
  Tailscale — spoken natively, so the runtime's context window, `keep_alive`,
  and `think` settings reach the server; set `LIGUS_OLLAMA_WIRE=openai` to force
  the OpenAI-compatible shim), and **any OpenAI-compatible endpoint**.
- **Subprocess agents** — CLI coding agents run as child processes and
  participate as first-class agents: **aider**, **pi**, **Codex** (OpenAI's
  `codex exec`), and any stdin-driven CLI via the generic **Hermes** runtime.

Each agent carries its own identity, role, and runtime, so a BYOA model knows
exactly who it is and who else is in the room.

### Proposals & review

Agents don't touch your machine on their own. A risky action — a file write, a
shell command — becomes a **proposal** that lands in the **Review** pane.
Approval uses **quorum voting**: a proposal can require more than one approval
and an optional **role floor** (only up-votes from members at or above a given
role count), and **self-approval is disallowed**. Reaching quorum still doesn't
auto-run anything — a human clicks **Implement** at the last mile.

A file-mutating agent turn runs in its **own isolated git worktree** (branched
from `HEAD`), so concurrent turns never clobber each other's uncommitted work.
Its diff is captured as a review-gated proposal — you see the exact patch in the
Review pane, and **Implement** applies it to your tree (`git apply`). Agents get
planning authority and can **author** proposals, never execution authority.

### Agentic workflows

Compose agents into **DAG pipelines** where each stage is either an agent turn
or a **human approval gate**. Stages with no unmet dependencies run in parallel;
gates pause the run until enough people approve. **Agents write them**: ask an
agent in chat for the pipeline you want, on any runtime, and its definition
arrives in the Review pane as a proposal; approving it adds the workflow, and
an agent can be asked to revise one the same way. The Workflows pane shows each
definition's stages and lets you run, pause or delete it. Definitions and run
records sync **end-to-end encrypted**, so teammates watch a run progress live and
vote its gates **from their own devices** (or their phone); a run **survives an
app restart** (**Resume** picks up where it left off, finished stages keeping
their outputs).

### Headless agents & always-on workers

Run an agent **without the desktop app** via the [`ligus` CLI](crates/hive-cli/README.md)
— on a cloud VM, a spare box, or as a background process. A **worker daemon**
registers a machine as a host and runs every agent bound to it on a
**workspace-owned credential** (never a personal key). Because it's always on, it
**drains a queue**: an `@mention` addressed to an agent while its host was offline
waits, and the worker answers the whole backlog when it comes up. The desktop app
surfaces the same queue and offers a **Run on worker** handoff for an offline
agent. Run **several workers** to drain the backlog in parallel — each claims
distinct turns, and per-turn worktrees keep their file edits from colliding. See
[Headless agents & setups](https://docs.ligus.ai/concepts/headless-agents/)
for the four topologies.

### Skills & vaults

Reusable **skills** (packaged instructions/workflows) and **vaults** (GitHub /
GitLab / HTTPS knowledge sources) extend what agents can do and know. Both — like
runtimes, agents, and MCP servers — can be **scoped per-agent or workspace-wide**,
so a workspace can share one roster of capabilities that every chat inherits, or
give a single agent its own. **MCP** (Model Context Protocol) servers plug in the
same way and stay **inert until you explicitly enable them**.

### Reactions, mentions & notifications

Emoji **reactions** are a first-class, low-cost channel for people *and* agents
(agents emit `[[react: 👍]]` / `[[vote: 👍 👎]]` directives), and they sync as
commutative add/remove events so concurrent votes never clobber each other. When
a mention names you — by handle, a role you hold, an `@all` broadcast, or an agent's
`@you` in a reply to you —
Ligus raises a native notification and an in-app cue, **including on the addressed
member's own device** when the mention arrives over sync.

### Git integration & diffs

Agents work against your real working tree. Proposed changes render as **diffs**
in the app before you approve them, and turns are attributed so you can see which
agent (or teammate) produced which change. Git context feeds the agents' prompts
so they reason about the actual state of the repo.

## How it works

### Event-sourced, per-device state

A Ligus workspace is an **append-only log of signed events** (`SessionEvent`s) in
a local SQLite database (`hive.db`). Nothing is a mutable row you edit in place;
every message, reaction, proposal, membership change, and config edit is an event
appended to the log. The UI state you see is a **projection** — the log **folded**
by a deterministic projector (`hive-core`). Because the fold is deterministic and
the log is the source of truth, every device that has the same events converges
on exactly the same state, which is what makes multi-device and multi-user
collaboration safe without a central database.

### E2EE sync topology

Syncing is just **replicating events** between devices. Each envelope is sealed
with ChaCha20-Poly1305 **on the device** before it goes anywhere. Two devices can
exchange them **directly peer-to-peer** (iroh) or through a **relay** that
**stores and forwards** sealed bodies keyed by workspace — the store-and-forward
is what lets an offline device (or an always-on worker) catch up on everything
pushed while it was away. The relay assigns a monotonic sequence and hands peers
everything after their cursor; it never sees plaintext and never holds a key.

### Monorepo layout

| Path | What it is |
|------|-----------|
| [`crates/hive-core`](crates/hive-core/README.md) | Pure, IO-free domain core: the event-sourced projector, `ChatSession`, authorization, E2EE primitives, and the proposal / workflow / skill / vault / channel types. The source of truth folded into state. |
| [`crates/hive-runtime`](crates/hive-runtime/README.md) | Orchestration over the core: SQLite event store, `ChatService`, sync engine + relay client, provider adapters, MCP, the tool loop, prompt building, vault fetcher. |
| [`crates/hive-proto`](crates/hive-proto/README.md) | The `ts-rs` DTOs shared with the frontend; a test regenerates `web/src/bindings/` so the TS contract can't drift. |
| [`crates/hive-cli`](crates/hive-cli/README.md) | The headless `ligus` client and worker daemon — the same runtime, no Tauri. |
| [`app/`](app/README.md) | The Tauri v2 desktop shell: IPC commands, `AppState`, and streaming events; bundles the web UI. |
| [`web/`](web/README.md) | The React 19 + TypeScript frontend (Vite, TanStack Query, Tauri IPC). |
| `addons/` | Optional agent shims (e.g. `aider-mcp`). |
| `deploy/` | Homebrew cask + distribution. |
| `docs/` | Developer & maintainer docs. |
| `docs-site/` | Published user/admin docs (MkDocs). |

The **production relay is a separate repo**,
**[github.com/ligus-ai/relay](https://github.com/ligus-ai/relay)** (Go) —
content-blind, hardened, and self-hostable. See
[`docs/architecture.md`](docs/architecture.md) for the deeper design.

## Security & privacy

Ligus is local-first, and anything that leaves the device is end-to-end encrypted.
The full model is documented in
[Security & trust](https://docs.ligus.ai/concepts/security/).

- **E2EE is mandatory for relay sync.** Every envelope is sealed with
  ChaCha20-Poly1305 before it leaves the device; the relay only ever holds sealed
  bodies. A workspace configured to sync but missing a key **refuses to push**
  rather than leaking plaintext — there is no "encrypt later" mode.
- **The relay is content-blind.** It stores and forwards ciphertext keyed by
  workspace. It cannot read your traffic, and because it never holds the key it
  cannot help recover a lost one.
- **The workspace key never transits the relay.** It's exchanged **out-of-band** —
  carried *inside* the `hivews1:` invite code, or sealed to an invitee's devices
  when you invite someone you already share a workspace with, so the relay is never
  a party to key exchange.
- **No sign-in provider.** Your account is a key made on your device with a random
  id; people check each other by safety code. Forge tokens (GitHub, GitLab, ...)
  stay in the OS secret store of the device doing git work and never reach a relay.
- **Authorization is enforced on ingest.** Governance is re-checked when a synced
  event is **projected**, not just when it was authored, deterministically on
  every device. A member therefore cannot forge a validly-signed
  `MemberRoleChanged{ self: Viewer → Owner }` to seize a workspace — the signature
  verifies, but the author's role didn't permit the change, so every peer drops
  it. Roles are **Owner / Admin / Contributor / Viewer**.
- **The founder is pinned via the invite.** The workspace founder is fixed at the
  root of trust, so authority can't be rewritten from the log.
- **Headless worker MCP tools are role-gated.** A synced message only drives a
  worker's MCP tools if its author meets a minimum role (default *contributor*),
  so a Viewer can't provoke a write to Linear/GitHub through a headless agent —
  it still replies, just tool-free.
- **Personal model keys never leave the device.** Detached agents run on a
  separate **workspace-owned** credential; your personal key stays local.

## Getting started

### Install

**Desktop app** — grab the installer for your platform from the
[**latest release**](https://github.com/ligus-ai/ligus/releases/latest):

| Platform | Download |
| --- | --- |
| **macOS** (Apple Silicon) | `Ligus_*_aarch64.dmg` |
| **Linux** | `Ligus_*_amd64.AppImage`, `.deb`, or `.rpm` |
| **Windows** | `Ligus_*_x64-setup.exe` or `.msi` |

On first launch, a five-step onboarding sets up your identity, project folder,
agent/runtime, team relay (optional), and appearance — no config files required.

**CLI &amp; headless worker** (`ligus`) — a one-line install for the terminal /
server side (macOS + Linux, Apple Silicon and x86_64):

```sh
curl -fsSL https://raw.githubusercontent.com/ligus-ai/ligus/main/install.sh | sh
```

It fetches the prebuilt `ligus` binary for your platform from the latest release.
Override the target dir with `LIGUS_INSTALL_DIR` or pin a tag with `LIGUS_VERSION`.
Or build from source: `cargo install --path crates/hive-cli`.

Use `ligus worker` to run a **headless daemon** that answers `@mentions` for the
agents it hosts — the production way to keep agents responsive when no desktop is
open. It runs on **workspace-owned** credentials (never a personal key, §12.5);
see [Configure a worker](#run-a-headless-worker) below.

> **Status.** Ligus is at its latest release (`1.1.1`). Installers are
> unsigned for now, so macOS/Windows may warn on first open. A Homebrew cask is
> being prepared under [`deploy/homebrew`](deploy/homebrew).

### A first workspace

1. **Launch and sign in.** Onboarding creates your local signing identity and
   picks the project folder agents will work in.
2. **Configure a runtime and an agent.** Point Ligus at a runtime — the `claude`
   CLI (no key needed), an Anthropic/OpenAI/OpenRouter/Ollama endpoint, or a
   subprocess agent — and its models populate automatically. Add a workspace
   agent bound to that runtime.
3. **Chat.** Open a chat and `@mention` your agent. Review any proposed file
   writes or commands in the **Review** pane and click **Implement** to run them.
4. **Invite a teammate (optional).** Share a workspace by handing over its
   `hivews1:` invite code **out-of-band** (paste it over a channel you trust) or
   by inviting someone you already share a workspace with. The invite carries the workspace key; the relay
   never sees it. Both of you then sync the same encrypted log.
5. **Go always-on (optional).** Run the [`ligus` CLI](crates/hive-cli/README.md)
   worker daemon on a spare box so an agent keeps answering while your laptop is
   closed.

Full user and self-hosting docs live at
**[docs.ligus.ai](https://docs.ligus.ai/)** (source in
[`docs-site/`](docs-site/)):

- [First launch](https://docs.ligus.ai/getting-started/first-launch/) ·
  [Configuring a runtime](https://docs.ligus.ai/getting-started/configuring-a-runtime/) ·
  [First chat](https://docs.ligus.ai/getting-started/first-chat/)
- [Multi-agent](https://docs.ligus.ai/features/multi-agent/) ·
  [Workflows](https://docs.ligus.ai/features/workflows/) ·
  [Collaboration](https://docs.ligus.ai/features/collaboration/)

## Run a headless worker

`ligus worker` is the production daemon: it registers the box as a workspace host
and answers `@mentions` for the agents assigned to it, so agents stay responsive
with no desktop open. It's configured entirely by environment variables:

| Var | Purpose |
| --- | --- |
| `LIGUS_DATA_DIR` | Local event-store directory. |
| `LIGUS_RELAY_URL` | Relay base URL (sync). |
| `LIGUS_RELAY_ACCESS_TOKEN` | Relay access token. |
| `LIGUS_WORKSPACE` | Workspace room id to serve. |
| `LIGUS_WORKSPACE_KEY` | The workspace E2EE key (so it can read/write the log). |
| `LIGUS_WS_SECRET_<ref>` | Secret for a workspace-owned runtime credential (§12.5). |
| `LIGUS_MCP_CONFIG` | Optional MCP server config for tool use. |

```sh
LIGUS_RELAY_URL=… LIGUS_RELAY_ACCESS_TOKEN=… LIGUS_WORKSPACE=… LIGUS_WORKSPACE_KEY=… \
  ligus worker
```

Assign agents to the worker from the desktop (or `ligus set-agent-host`). The
worker uses **workspace-owned** runtime credentials — never a personal key — and
gates MCP tool use on the triggering author's role (`LIGUS_MCP_MIN_ROLE`, default
contributor). It claims each turn before answering, so a worker and a desktop
that both own an agent never double-reply, and it recovers from transient relay
outages with backoff. Multiple agents can carry on a conversation across worker
and desktop without a human in the loop, up to a generous per-turn cascade
ceiling (a loop backstop, not a conversation limit).

> `ligus agent` is a **local-dev-only** foreground responder that runs on a
> personal credential; it refuses to start unless `LIGUS_ALLOW_PERSONAL_AGENT=1`
> is set. For anything unattended or shared, use `ligus worker`.

## Deploying with an agent

A coding agent can deploy Ligus for you, on Kubernetes or on this machine.
[`AGENTS.md`](AGENTS.md) has the decision table, the exact commands and the
rules it follows (no secret values in chat or files, never weakening the
sandbox, NetworkPolicies or TLS, stopping for passkeys, invites, exposure and
DNS/TLS). The step-by-step skills are
[`.claude/skills/ligus-deploy-k8s`](.claude/skills/ligus-deploy-k8s/SKILL.md)
and [`.claude/skills/ligus-deploy-local`](.claude/skills/ligus-deploy-local/SKILL.md);
the tools are [`deploy/kubernetes/ligus-k8s`](deploy/kubernetes/ligus-k8s)
and [`deploy/local/ligus-local`](deploy/local/ligus-local) (non-interactive,
idempotent, `--json`). See also
[Deploying with an agent](https://docs.ligus.ai/deploy/with-an-agent/).

## Self-hosting the relay

Collaboration never requires *our* servers. The relay is a single small,
content-blind binary in its own repo,
**[github.com/ligus-ai/relay](https://github.com/ligus-ai/relay)** (Go,
MIT) — which carries the Docker image and deploy manifests. The shortest smoke
test:

```bash
docker build -t ligus-relay https://github.com/ligus-ai/relay.git && \
  docker run -p 8443:8443 ligus-relay
curl http://localhost:8443/v1/health      # → ok
```

Bind address and bounds are env-driven:

| Variable | Default | Purpose |
| --- | --- | --- |
| `$PORT` / `LIGUS_RELAY_ADDR` | `0.0.0.0:8443` | Bind address (`$PORT` for PaaS, or a full `host:port`). |
| `LIGUS_RELAY_RETENTION_DAYS` | `0` (off) | Prune envelopes older than N days. |
| `LIGUS_RELAY_MAX_BODY_BYTES` | `4194304` (4 MiB) | Max request body size. |
| `LIGUS_RELAY_ACCESS_TOKENS` | unset (open) | Optional comma-separated allowlist gating *who can connect*. |

`/v1/health` round-trips the store, so it reports real liveness. See
[Self-hosting a relay](https://docs.ligus.ai/networking/self-host/) for a full
VM/PaaS deploy, TLS, and access management.

## Build from source

Prerequisites: **Rust** (via [rustup](https://rustup.rs)), **[Bun](https://bun.sh)**,
the [Tauri v2 system deps](https://tauri.app/start/prerequisites/) for your OS,
and the [`tauri-cli`](https://tauri.app/reference/cli/) (`cargo install tauri-cli`).

```bash
cargo test --workspace                   # Rust crates + the Tauri app
cd web && bun install && bun run build    # frontend: typecheck (tsc) + build
cargo tauri dev                          # run the app (frontend + backend)
```

> To exercise sync locally, run a relay from the standalone Go service —
> [github.com/ligus-ai/relay](https://github.com/ligus-ai/relay) (see
> [Self-hosting the relay](#self-hosting-the-relay) for the one-liner).

Packaged installers are produced by [`scripts/build.sh`](scripts/build.sh) (e.g.
`./scripts/build.sh mac`). See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full
dev loop and [`docs/architecture.md`](docs/architecture.md) for how the pieces
fit together.

## Configuration

- **The app is config-free to start** — onboarding sets identity, project folder,
  runtime, and relay; there are no required config files.
- **Repo settings** live in [`ligus.config.toml`](ligus.config.toml) at the root
  (workspace state lives in `.ligus/`). Ligus 2.0 starts fresh: a Hive 1.x
  `hive.config.toml`, `.hive/`, `~/.hive` or desktop data dir is never read,
  moved or changed; see
  [Upgrading from Hive 1.x](docs-site/deploy/upgrade-from-hive.md) for how to
  remove 1.x when you're done.
- **Headless clients** are entirely **environment-driven** so they drop into a
  container or secret mount — see the
  [`ligus` CLI README](crates/hive-cli/README.md) for every `LIGUS_*` variable
  (relay URL/token, workspace + key, provider/model, MCP config, role gate).
- **IPC types** in [`hive-proto`](crates/hive-proto/README.md) generate the
  TypeScript bindings in `web/src/bindings/`; regenerate with
  `cargo test -p hive-proto export_bindings`.

## Editions

The desktop app (this repo) and the production relay (the standalone
[Go service](https://github.com/ligus-ai/relay)) are both open source and
self-hostable. A managed / enterprise tier adds server-side membership & RBAC,
hosted relays, and org (GHEC) integration — those paid controls live outside this
repo and attach via a small `WriteGuard` extension seam, so self-hosters keep a
fully functional, content-blind relay. See
[the tiering model](docs-site/ops/tiering.md).

## Contributing & security

Issues and PRs are welcome — start with [`CONTRIBUTING.md`](CONTRIBUTING.md) and
our [Code of Conduct](CODE_OF_CONDUCT.md). Developer and architecture docs live
in [`docs/`](docs/README.md). To report a vulnerability, follow
[`SECURITY.md`](SECURITY.md) (please don't open a public issue for security bugs).

## Why Ligus?

Ligus is short for *ligustica*. *Apis mellifera ligustica* is the Italian
honeybee, the bees our maintainer keeps, and *Ligus* is Latin for "a Ligurian,"
the region the subspecies is named for. We were Hive, and we still like the
image: a colony that does its work through many bees in one shared space, each
one useful on its own and more useful together. The new name keeps the idea and
points at the bees that inspired it. (Pronounced LEE-gus.) The default agent is
still called `@hive`: you are talking to the hive.

## License

[MIT](LICENSE).

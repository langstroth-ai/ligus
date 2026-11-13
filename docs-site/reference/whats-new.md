# What's new

**Hive is now Ligus (v2.0.0).** Ligus is short for *ligustica*, the Italian
honeybee. Entries for 1.19 and earlier keep the name Hive.

Changes since 1.15, newest first, with links to where each is documented.
Full release notes are on the
[GitHub releases page](https://github.com/ligus-ai/ligus/releases).

## 2.0

- **Hive is now Ligus.** The code, protocol and your data are unchanged:
  workspaces, devices, relays and paired phones keep working, and the default
  agent is still `@hive`.
- **Desktop data moves over on first launch.** The app's identifier is now
  `ai.ligus.desktop`. On first launch Ligus copies your data from the
  `com.hive.desktop` folder into its new folder and leaves the old one
  untouched; your device identity and keys come along, so nothing needs
  re-pairing. Theme and pane layout reset once, and macOS asks again for
  folder and notification access. An auto-updated app may still be at
  `/Applications/Hive.app`; you can rename it to `Ligus.app`.
- **`ligus` command.** The CLI is now `ligus`. The `hive` command still works
  through v2.1 and prints a one-line reminder on stderr. Homebrew: formula
  `ligus`, cask `ligus-app`, tap `ligus-ai/ligus`; existing `hive-cli` and
  `hive` installs upgrade to them.
- **`LIGUS_*` settings.** Environment variables are now `LIGUS_*`. The
  `HIVE_*` names still work (each prints a one-time note) and lose to the
  `LIGUS_*` name when both are set.
- **New file names.** New workspace state goes in `.ligus/` and new config in
  `ligus.config.toml`; the CLI's default data dir is `~/.ligus`. Existing
  `.hive/`, `hive.config.toml` and `~/.hive` are still read and never moved.
  See [Configuration](config-toml.md).
- **New addresses.** The project lives at github.com/ligus-ai/ligus and the
  docs at docs.ligus.ai. Old links redirect. Protocol identifiers that contain
  `hive` keep their names; see [Wire protocols](wire-protocols.md#legacy-protocol-identifiers).

## 1.19

- **Hive is becoming Ligus.** The next major release, 2.0, renames Hive to
  **Ligus**, short for *ligustica* (*Apis mellifera ligustica*, the Italian
  honeybee). 1.19 is the bridge: it already accepts `LIGUS_*` environment
  variables (ahead of the `HIVE_*` names), reads `.ligus/` and
  `ligus.config.toml` alongside `.hive/` and `hive.config.toml`, accepts
  `ligus://` phone links, and checks for updates at both the new and the
  original GitHub address. Nothing to change on your side yet.
- **Repos from the phone.** `hive serve` lists the host's checkouts and your
  GitHub repos, clones on demand, switches a chat's repo, pulls, and opens a
  pull request from the chat branch. See
  [Repos on the home host](../ops/serve-repos.md).
- **Fork-aware pull requests.** For a fork with an `upstream` remote, chat
  branches start from upstream's default branch, pulls come from upstream,
  and PRs are pushed to your fork under a readable branch and opened upstream
  as drafts. Pushes that would land on upstream are refused.
- **Whole files for rendered diffs.** `GET /v1/proposals/{id}/file` returns
  one file of a proposal whole (old or new side), so the phone app can render
  Markdown changes instead of showing raw diff lines.
- **Kubernetes design and a container image.** A design for running the relay,
  `hive serve` and sandboxed turn executors on Kubernetes, and a Dockerfile for
  the `hive` binary (`deploy/container`).

## 1.18

- **Agent-authored workflows.** The visual workflow editor and its presets
  are gone. Agents write workflows: a definition (or a revision of one)
  arrives in the Review pane as a proposal, and approving it adds it. The
  Workflows pane is read-only, with Run, Pause/Resume, Delete and *Ask an
  agent to change this*. Local-model agents get a `propose_workflow` tool
  that checks the definition in the same turn. Existing workflows keep
  working. See [Agentic workflows](../features/workflows.md).
- **Google Antigravity and GitHub Copilot runtimes.** Use your Google AI
  Pro/Ultra or Copilot subscription through the `agy` and `copilot` CLIs;
  Settings → Models detects whether each is installed and signed in. See
  [Configuring a runtime](../getting-started/configuring-a-runtime.md) and
  [Bring-your-own agents](../addons/byo-agents.md).

## 1.17

- **Host dashboard.** `hive serve` serves a dashboard to the phone: a
  built-in Hive section (agents, proposals, workflows, turns, host, relay and
  push status), your own panels from `dashboard.toml`, panels backed by MCP
  tools, a table panel kind, and `hive dashboard check | metrics | default`.
  See [Host dashboard](../features/dashboard.md).
- **MCP tools at the local-model tool cap.** Local agents no longer silently
  lose MCP tools at the 16-tool limit: slots are kept for MCP tools, tools
  you name are preferred, and dropped tools are logged. MCP errors show the
  server's own message. See [MCP servers](../addons/mcp.md).

## 1.16.1

- Every user message a sync brings in gets an answer, not only the chat's
  last one.
- Push previews are plain text.
- `hive remote status` shows how long the relay link has been up and
  notices a `hive serve` that stopped; `hive --version` prints the version.
  See [Troubleshooting](../ops/troubleshooting.md).

## 1.16

- **Hive on your phone, from anywhere.** `hive serve` plus the relay tunnel
  gives the phone app remote access without a VPN: `hive pair` prints the
  pairing link, `hive phones` lists and revokes phones, `hive remote`
  manages the link. A self-hosted relay can run the tunnel and send push
  with its own APNs key. See [Remote access for your phone](../networking/remote-access.md).
- **On the phone:** people, channels and chat details; proposal review
  with votes (who and role), a refinement thread and *request changes*;
  push for mentions and workflow approvals; the host's theme through
  *Match host*. See [Hive on your phone](../features/phone-app.md).
- **The serve box as your own device.** `hive serve enroll` makes the box
  one of your devices instead of a separate member. See
  [Running hive serve as your own device](../ops/serve-box.md).
- **Sync privacy fix.** A relay room now carries only its own workspace;
  events from other workspaces are dropped and purged on upgrade. Read the
  [upgrade note](../ops/serve-box.md#what-to-expect-and-do-when-upgrading-from-115-or-earlier)
  if you're coming from 1.15 or earlier.
- **Presence-based host liveness.** Whether a host is online rides the
  relay's presence channel instead of heartbeat events in the log. See
  [Host liveness](../ops/serve-box.md#host-liveness).

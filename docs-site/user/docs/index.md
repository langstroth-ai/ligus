# ![Ligus](images/hive-lockup-light.svg#only-light){ .hero-lockup }![Ligus](images/hive-lockup-dark.svg#only-dark){ .hero-lockup }

**A shared LLM workspace for developers.** Bring your own subscription
(Claude, OpenAI, OpenRouter, Ollama, aider, pi, or any local agent),
collaborate with peers on real work — code, commits, reviews — and
keep your data local-first.

*Ligus* is short for *ligustica* (*Apis mellifera ligustica*, the Italian
honeybee). The project was called Hive until v2.0; old links to
docs.apiaryhq.ai redirect here.

![The Ligus workspace](images/overview.png){ width="900" }

---

## Why Ligus

Most "AI for code" tools assume one developer, one model, one
machine. Ligus flips that:

- **Multiple peers, one workspace.** Alice's Claude subscription and
  Bob's Ollama instance can both participate in the same chat,
  reviewing each other's proposals.
- **Bring your own agent.** Aider, pi, Claude Code, Codex, Google
  Antigravity, GitHub Copilot, or any CLI agent
  you trust can act as a workspace participant. Ligus doesn't recreate
  agent logic — it hosts what you bring.
- **Local-first, P2P sync.** Workspaces sync directly between peers
  over a tiny optional relay. No central server holds your content.
- **Nothing runs behind your back.** Installed MCP servers stay inert
  until you enable them, Claude Code respects its own permission mode,
  and agent changes arrive as proposals you review before implementing.

---

## What's in here

<div class="grid cards" markdown>

-   :material-rocket-launch:{ .lg } **Getting started**

    First launch, configuring runtimes, your first chat.

    [:octicons-arrow-right-24: Start here](getting-started/first-launch.md)

-   :material-server-network:{ .lg } **Deploy**

    Solo, personal fleet with a phone, a team on a relay, Kubernetes.

    [:octicons-arrow-right-24: Choose a deployment](deploy/index.md)

-   :material-shield-lock:{ .lg } **Security**

    What each component holds, the threat model, incident runbooks.

    [:octicons-arrow-right-24: Security model](security/index.md)

-   :material-graph:{ .lg } **Concepts**

    Workspaces, agents, MCP tools, permissions, identity.

    [:octicons-arrow-right-24: Read the model](concepts/workspaces.md)

-   :material-server-network:{ .lg } **Networking & relay**

    How peers find each other; setting up your own relay.

    [:octicons-arrow-right-24: Networking](networking/rendezvous.md)

-   :material-puzzle:{ .lg } **Addon agents**

    Wire aider, pi, or any subprocess agent as a workspace participant.

    [:octicons-arrow-right-24: BYO agents](addons/byo-agents.md)

</div>

---

## Built with

Ligus is a **Tauri v2** desktop app — a Rust backend (`crates/hive-core`,
`hive-runtime`, `hive-cli`, plus `app/`) with a React + TypeScript
frontend (`web/`). One codebase ships **macOS, Windows, and Linux**. State is an
append-only event log in SQLite; replies stream from your chosen runtime (the
`claude` CLI by default — bring your own subscription).

## Project status

Ligus 2.0 is the current release (it was called Hive until 1.18; 2.0 starts fresh and leaves Hive 1.x data
untouched, see [Upgrading from Hive 1.x](deploy/upgrade-from-hive.md)). The macOS app is
signed and notarized and updates itself; Windows and Linux builds are
unsigned. The headless `ligus` CLI ships for macOS and Linux through
Homebrew. See [What's new](reference/whats-new.md).

The reference relay is MIT-licensed and lives in its own repository,
[github.com/ligus-ai/relay](https://github.com/ligus-ai/relay) (a small Go
service); see [Self-hosting a relay](networking/self-host.md). Desktops that
are contacts also sync directly peer to peer over **iroh**, falling back to
the relay. Kubernetes support is in preview
([Kubernetes](deploy/kubernetes.md)).

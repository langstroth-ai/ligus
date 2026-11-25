# ![Ligus](images/ligus-lockup-light.svg#only-light){ .hero-lockup }![Ligus](images/ligus-lockup-dark.svg#only-dark){ .hero-lockup }

**A shared LLM workspace for developers.** Bring your own subscription
(Claude, OpenAI, OpenRouter, Ollama, aider, pi, or any local agent),
collaborate with peers on real work — code, commits, reviews — and
keep your data local-first.

*Ligus* is short for *ligustica* (*Apis mellifera ligustica*, the Italian
honeybee).

This section covers using Ligus: the desktop and phone apps, workspaces,
agents, the `ligus` CLI. Self-hosting and deploying (the relay, sign-in,
Kubernetes, always-on `ligus serve` hosts) is in
**[Operating Langstroth](../operate/index.md)**.

![The Ligus desktop app: a chat where agents hand work to each other, and the Review pane with a proposal, its diff and one of two approvals (demo data)](images/desktop-overview-light.png#only-light){ loading=lazy }
![The Ligus desktop app: a chat where agents hand work to each other, and the Review pane with a proposal, its diff and one of two approvals (demo data)](images/desktop-overview-dark.png#only-dark){ loading=lazy }

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

-   :material-server-network:{ .lg } **Self-host and deploy**

    Solo, personal fleet with a phone, a team on a relay, Kubernetes. In
    Operating Langstroth.

    [:octicons-arrow-right-24: Choose a deployment](../operate/deploy/index.md)

-   :material-shield-lock:{ .lg } **Security**

    What each component holds, the threat model, incident runbooks (in
    Operating Langstroth). For the short version, see
    [Security & trust](concepts/security.md).

    [:octicons-arrow-right-24: Security model](../operate/security/index.md)

-   :material-graph:{ .lg } **Concepts**

    Workspaces, agents, MCP tools, permissions, identity.

    [:octicons-arrow-right-24: Read the model](concepts/workspaces.md)

-   :material-cellphone:{ .lg } **Ligus on your phone**

    Chats, proposals and approvals from the iPhone app, through your own
    always-on host.

    [:octicons-arrow-right-24: Phone app](features/phone-app.md)

-   :material-puzzle:{ .lg } **Addon agents**

    Wire aider, pi, or any subprocess agent as a workspace participant.

    [:octicons-arrow-right-24: BYO agents](addons/byo-agents.md)

</div>

---

## Built with

Ligus is a **Tauri v2** desktop app — a Rust backend (`crates/ligus-core`,
`ligus-runtime`, `ligus-cli`, plus `app/`) with a React + TypeScript
frontend (`web/`). One codebase ships **macOS, Windows, and Linux**. State is an
append-only event log in SQLite; replies stream from your chosen runtime (the
`claude` CLI by default — bring your own subscription).

## Project status

Ligus 0.3.0 is the current release. The macOS app is
signed and notarized and updates itself; Windows and Linux builds are
unsigned. The headless `ligus` CLI ships for macOS and Linux through
Homebrew. See [What's new](reference/whats-new.md).

The reference relay is MIT-licensed and lives in its own repository,
[github.com/langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay) (a small Go
service); see [Self-hosting a relay](../operate/networking/self-host.md). Desktops that
are contacts also sync directly peer to peer over **iroh**, falling back to
the relay. Kubernetes support is in preview
([Kubernetes](../operate/deploy/kubernetes.md)).

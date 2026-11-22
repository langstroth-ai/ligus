# Ligus developer & maintainer docs

Docs for people working **on** Ligus (architecture, internals, contributing).
If you're looking for how to **use** or **self-host** it, that's the published
MkDocs site, docs.langstroth.ai, built from [`docs-site/`](../docs-site/README.md).

> **Audience split.** `docs-site/docs/ligus/` = using the Ligus apps (Using
> Ligus, docs.langstroth.ai/ligus/). `docs-site/docs/operate/` = self-hosting
> and deploying the Langstroth platform (Operating Langstroth,
> docs.langstroth.ai/operate/). `docs/` (here) = implementation, internals,
> release engineering (contributors). Rule of thumb: *"how do I use it"* →
> Using Ligus; *"how do I run/deploy it"* → Operating Langstroth; *"how is it
> built / how do I change it"* → here.

## Start here

| Doc | What it covers |
|-----|----------------|
| [architecture.md](architecture.md) | Crate layout, event sourcing + projector, Tauri IPC + ts-rs, provider dispatch. |
| [multiuser.md](multiuser.md) | Relay-forwarded sync, E2EE, testing multiuser across machines. |
| [relay-deploy.md](relay-deploy.md) | Cloud-hosting the `langstroth-relay` binary. |
| [packaging.md](packaging.md) | Bundling + signing/notarization per OS. |
| [ligus-server-side-membership.md](../docs-site/docs/operate/ops/server-side-membership.md) | Relay-side workspace membership in the open relay (`LANGSTROTH_RELAY_MEMBERSHIP=1`). |

## Feature plans

| Doc | What it covers |
|-----|----------------|
| [ligus-social-graph-plan.md](ligus-social-graph-plan.md) | Add-friend-by-GitHub-username, presence, P2P bootstrap; GitHub Teams → roster and roles. |
| [ligus-issue-trackers.md](ligus-issue-trackers.md) | Per-workspace issue/ticket integration via MCP (GitHub Issues/PRs, Linear, …). |
| [android-app.md](android-app.md) | Android app spec: parity with the iPhone app, Keystore device key, Credential Manager passkeys, FCM push (relay change), shared Rust core via UniFFI, milestones, design and experience notes (Material 3, notifications, accessibility). |

## Design intent & internals

Deeper design references — useful context for extending Ligus; when one conflicts
with the code, the code wins.

- Agent model: [`ligus-agent-architecture-matrix.md`](ligus-agent-architecture-matrix.md) — agent modes, execution/state/tool/permission shape.
- Relay protocol: [`langstroth-relay-api.md`](langstroth-relay-api.md) — the `/v1` wire contract (client ↔ relay).
- Phone remote access: [`relay-tunnel.md`](relay-tunnel.md) — the open `hive-tunnel/1` protocol (host registration, phone credentials, Noise end-to-end session, stream mux, push). Golden vectors and their Go generator and Rust cross-checker are in [`relay-tunnel/`](relay-tunnel/). Where it's built: relay endpoints in [langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay), the host connector in `crates/ligus-cli/src/serve/tunnel/`, the phone client in the `ligus-ios` repo, and APNs push in the relay.
- Proposals and collaboration (design notes, not implemented; each ends with its adversarial assessment):
  [`design/proposal-line-comments.md`](design/proposal-line-comments.md) (line comments in the refinement thread),
  [`design/review-screen-approve-and-apply.md`](design/review-screen-approve-and-apply.md) (the Review screen and "Approve & apply"),
  [`design/shared-live-sessions.md`](design/shared-live-sessions.md) (presence, steering, hand-over, comments on a running turn),
  [`design/devices-and-keys.md`](design/devices-and-keys.md) (Devices & keys; the fingerprint word form is deferred),
  [`design/update-notices.md`](design/update-notices.md) (daily update notices and the unreadable-events warning),
  [`design/lt-clipboard-copy.md`](design/lt-clipboard-copy.md) (copying from `lt`, invite codes included).
- Kubernetes: [`design/kubernetes.md`](design/kubernetes.md) — relay, `ligus serve` and agent runners on Kubernetes, sandboxed turn executors, security model, phased plan (design; Phase 1 chart in `deploy/helm/ligus/`, image in `deploy/container/`).
- Ligus 0.1: [`design/v2/`](design/v2/README.md) — architecture review, Phase 0 track specs, identity without GitHub and many forges, relay-directory trust.
- Branding: [`ligus-logo-system.md`](ligus-logo-system.md) — logo/brand assets.

Operator docs (self-hosting the relay, langstroth-auth, hosts) live under
[`docs-site/docs/operate/`](../docs-site/docs/operate/).

## Building & running

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the quick build/test loop. In short:

```bash
cargo test --workspace            # Rust
cd web && bun run build           # frontend typecheck + build
cargo tauri dev                   # run the app
cargo run -p langstroth-relay           # run the relay
```

# Hive developer & maintainer docs

Docs for people working **on** Hive (architecture, internals, contributing).
If you're looking for how to **use** or **self-host** it, that's one of the two
published MkDocs sites built from [`docs-site/`](../docs-site/README.md).

> **Audience split.** `docs-site/user/` = using the Ligus apps (docs.ligus.ai).
> `docs-site/operator/` = self-hosting and deploying the Langstroth platform
> (docs.langstroth.ai). `docs/` (here) = implementation, internals, release
> engineering (contributors). Rule of thumb: *"how do I use it"* → user site;
> *"how do I run/deploy it"* → operator site; *"how is it built / how do I
> change it"* → here.

## Start here

| Doc | What it covers |
|-----|----------------|
| [architecture.md](architecture.md) | Crate layout, event sourcing + projector, Tauri IPC + ts-rs, provider dispatch. |
| [multiuser.md](multiuser.md) | Relay-forwarded sync, E2EE, testing multiuser across machines. |
| [relay-deploy.md](relay-deploy.md) | Cloud-hosting the `hive-relay` binary. |
| [packaging.md](packaging.md) | Bundling + signing/notarization per OS. |
| [hive-server-side-membership.md](../docs-site/operator/docs/ops/server-side-membership.md) | Server-enforced workspace membership (the `WriteGuard` seam + the private enterprise crate). |

## Product & tiers

| Doc | What it covers |
|-----|----------------|
| [tiering.md](../docs-site/operator/docs/ops/tiering.md) | OSS core vs paid/enterprise — how features gate (entitlements vs build) and where they live. |
| [managed-service-plan.md](../docs-site/operator/docs/ops/managed-service.md) | GitHub-account identity, hosted relay, signed entitlement tokens, RBAC/GHEC roadmap. |
| [hive-social-graph-plan.md](hive-social-graph-plan.md) | Add-friend-by-GitHub-username, presence, P2P bootstrap; GitHub Teams for Enterprise; the prosumer paid tier. |
| [hive-issue-trackers.md](hive-issue-trackers.md) | Per-workspace issue/ticket integration via MCP (GitHub Issues/PRs, Linear, …). |

## Design intent & internals

Deeper design references — useful context for extending Hive; when one conflicts
with the code, the code wins.

- Agent model: [`hive-agent-architecture-matrix.md`](hive-agent-architecture-matrix.md) — agent modes, execution/state/tool/permission shape.
- Relay protocol: [`hive-relay-api.md`](hive-relay-api.md) — the `/v1` wire contract (client ↔ relay).
- Phone remote access: [`relay-tunnel.md`](relay-tunnel.md) — the open `hive-tunnel/1` protocol (host registration, phone credentials, Noise end-to-end session, stream mux, push). Golden vectors and their Go generator and Rust cross-checker are in [`relay-tunnel/`](relay-tunnel/). Where it's built: relay endpoints in [ligus-ai/relay](https://github.com/ligus-ai/relay), the host connector in `crates/hive-cli/src/serve/tunnel/`, the phone client in the private `ligus-ios` repo, and billing/APNs in the private `ligus-ai/hive-cloud`.
- Kubernetes: [`design/kubernetes.md`](design/kubernetes.md) — relay, `ligus serve` and agent runners on Kubernetes, sandboxed turn executors, security model, phased plan (design; Phase 1 chart in `deploy/helm/ligus/`, image in `deploy/container/`).
- Ligus 2.0: [`design/v2/`](design/v2/README.md) — architecture review, Phase 0 track specs, identity without GitHub and many forges, relay-directory trust.
- Branding: [`hive-logo-system.md`](hive-logo-system.md) — logo/brand assets.

The tiering and managed-service docs live on the operator site under
[`docs-site/operator/docs/ops/`](../docs-site/operator/docs/ops/); pricing is on
the user site at [`docs-site/user/docs/help/pricing.md`](../docs-site/user/docs/help/pricing.md).

## Building & running

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the quick build/test loop. In short:

```bash
cargo test --workspace            # Rust
cd web && bun run build           # frontend typecheck + build
cargo tauri dev                   # run the app
cargo run -p hive-relay           # run the relay
```

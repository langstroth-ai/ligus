# Operating Langstroth

**Langstroth** is the platform behind the Ligus apps: the end-to-end
encrypted relay, organization sign-in (ligus-auth), the Helm charts, and the
always-on Ligus hosts (`ligus serve` and `ligus worker`). This section is for
the people who self-host and run it. Using the Ligus desktop app, the phone
app and the `ligus` CLI day to day is in **[Using Ligus](../ligus/index.md)**.

*Ligus* is short for *ligustica* (*Apis mellifera ligustica*, the Italian
honeybee). Commands, chart names and environment variables still use the
`ligus` and `LIGUS_*` names; this section documents them as they ship.

## Two things to know first

- **Workspaces are local-first.** Every device (a desktop app, a
  `ligus serve` box, a worker) keeps its own copy of the workspace as a signed
  event log, and its own keys. One data dir is one device. See
  [Workspaces & chats](../ligus/concepts/workspaces.md) in Using Ligus
  for the model.
- **The relay is content-blind.** It stores and forwards end-to-end
  encrypted event envelopes and sees ciphertext and metadata, never a
  workspace key. Whoever holds the invite can read the workspace. The user
  view is [Security & trust](../ligus/concepts/security.md); the
  operator view, with trust boundaries per deployment, is the
  [Security model](security/index.md).

## Where to start

| You want to | Read |
|---|---|
| Pick a shape: solo, personal fleet, team, Kubernetes | [Choose a deployment](deploy/index.md) |
| Hand the deployment to a coding agent | [Deploying with an agent](deploy/with-an-agent.md) (AGENTS.md and the deploy skills) |
| Run a relay (Docker, Fly.io, systemd), pick its storage, gate access | [Self-hosting a relay](networking/self-host.md) |
| Let people sign in with your identity provider | [ligus-auth](deploy/ligus-auth.md), with [Pocket ID](deploy/ligus-auth-pocket-id.md) or [Dex](deploy/ligus-auth-dex.md) |
| Run the relay and Ligus hosts on a cluster | [Kubernetes (preview)](deploy/kubernetes.md) |
| Route agents through your LLM gateway (LiteLLM, kgateway, Envoy AI Gateway) | [Deploying with an LLM gateway](deploy/model-gateways.md) |
| Run an always-on host for your phone and agents | [ligus serve as your own device](ops/serve-box.md), [Headless agents](concepts/headless-agents.md), [Remote access](networking/remote-access.md) |
| Monitor, back up, restore, upgrade | [Operations](ops/operations.md), [Upgrading from Hive 1.x](deploy/upgrade-from-hive.md) |
| Understand what an attacker can do | [Threat model](security/threat-model.md), [Runbooks](security/runbooks.md) |
| Look up a variable, a config key or an API route | [Environment variables](reference/env-vars.md), [Configuration (TOML)](reference/config-toml.md), [Serve HTTP API](reference/serve-api.md) |

For agents reading these docs: [llms.txt](../llms.txt) lists every page with a
one-line summary.

## Questions

Ask on GitHub: [issues](https://github.com/langstroth-ai/ligus/issues) and
[discussions](https://github.com/langstroth-ai/ligus/discussions) on
langstroth-ai/ligus, or
[langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay/issues)
for the relay.

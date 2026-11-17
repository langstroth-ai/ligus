---
hide:
  - navigation
  - toc
---

# Langstroth documentation

**Langstroth** is the open, self-hostable platform behind **Ligus**, a
shared LLM workspace for developers. *Ligus* is short for *ligustica*
(*Apis mellifera ligustica*, the Italian honeybee); the project was called
Hive until 2.0.

- **Ligus** is the app: the desktop app, the phone app and the `ligus` CLI.
  You bring your own subscription and your own agents, and work with people
  and agents in shared, end-to-end-encrypted workspaces.
- **Langstroth** is what the apps run on: the content-blind relay,
  organization sign-in (ligus-auth), the Helm charts, and always-on Ligus
  hosts (`ligus serve` and `ligus worker`). Everything is MIT-licensed and
  self-hosted.

These docs have two sections.

<div class="grid cards" markdown>

-   :material-account-group:{ .lg } **Using Ligus**

    Install the apps, configure a runtime, start a workspace, bring your own
    agents, review proposals, use Ligus from your phone.

    [:octicons-arrow-right-24: Using Ligus](ligus/index.md)

-   :material-server-network:{ .lg } **Operating Langstroth**

    Choose a deployment, run a relay, sign in with your identity provider,
    run always-on hosts, deploy to Kubernetes, and read the security model.

    [:octicons-arrow-right-24: Operating Langstroth](operate/index.md)

</div>

## Start here

| You want to | Read |
|---|---|
| Install Ligus and send a first message | [First launch](ligus/getting-started/first-launch.md), [Your first chat](ligus/getting-started/first-chat.md) |
| Understand the model: workspaces, agents, proposals | [Workspaces & chats](ligus/concepts/workspaces.md), [Agents](ligus/concepts/agents.md) |
| Use Ligus from your phone | [Ligus on your phone](ligus/features/phone-app.md), [Remote access](operate/networking/remote-access.md) |
| Pick a deployment shape | [Choose a deployment](operate/deploy/index.md) |
| Run your own relay | [Self-hosting a relay](operate/networking/self-host.md) |
| Know what the relay and each device can see | [Security & trust](ligus/concepts/security.md), [Security model](operate/security/index.md) |
| Move from Hive 1.x | [Upgrading from Hive 1.x](operate/deploy/upgrade-from-hive.md) |

For agents reading these docs: [llms.txt](llms.txt) lists every page with a
one-line summary.

## Questions

Ask on GitHub: [issues](https://github.com/langstroth-ai/ligus/issues) and
[discussions](https://github.com/langstroth-ai/ligus/discussions) on
langstroth-ai/ligus.

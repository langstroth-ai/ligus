# Ligus on Kubernetes

Status: **preview** (charts and images not published). The operator guide,
including supported nodes, backups, upgrades and what 2.0 does not support, is
[`docs-site/docs/operate/deploy/kubernetes.md`](../../docs-site/docs/operate/deploy/kubernetes.md). The Helm chart is in
[`../helm/ligus`](../helm/ligus) and its README is the install guide. The
design, and the phases still to come, are in
[`docs/design/kubernetes.md`](../../docs/design/kubernetes.md). The container
image for the `ligus` binary is in
[`../container/Dockerfile`](../container/Dockerfile).

## With an agent, or scripted

[`ligus-k8s`](ligus-k8s) runs preflight (with a `--probe` that measures Pod
Security, NetworkPolicy enforcement and Landlock), secret bootstrap, install
from a scenario in [`scenarios/`](scenarios), verify and teardown,
non-interactively with `--json`. See [`AGENTS.md`](../../AGENTS.md).

## What runs where

| Component | Kubernetes shape | State | Holds E2EE keys? |
|---|---|---|---|
| Relay (separate repo, `ligus-ai/relay`) | Deployment + Postgres (external or an in-cluster operator); the tunnel part pinned to one replica | Postgres | **No**, ciphertext only |
| `ligus serve` (one per workspace) | StatefulSet, 1 replica, PVC for `LIGUS_DATA_DIR` | PVC (identity, `hive.db`, tunnel keys, checkouts) | Yes |
| `ligus worker` | One single-replica StatefulSet per named host (`worker.instances[]`); agents bound by host | PVCs per host | Yes |
| MCP servers | Deployment + Service (or ToolHive / kagent CRDs); stdio servers run inside the turn | per server | No |
| Local models | vLLM / KServe / llm-d behind an OpenAI-compatible Service | model cache | No |

Two rules follow from Ligus's design and shape everything here:

1. **One identity, one writer.** A `ligus serve` or `ligus worker` process is a
   Ligus *device*: its keys and its SQLite event log live in its data dir. Never
   run two pods on the same data dir, and never put `hive.db` on a network
   filesystem (NFS, SMB, RWX volumes). Use a `ReadWriteOncePod` PVC and a
   StatefulSet.
2. **Keys stay out of the relay.** The workspace key, device keys and model or
   git credentials live only in the namespace of the workspace that owns them.
   The relay namespace never sees them.

## Building the image

```sh
docker build -f deploy/container/Dockerfile -t ligus:dev .
docker run --rm ligus:dev --version
```

The image runs as UID 65532, uses `/var/lib/ligus` as `LIGUS_DATA_DIR`, and
contains git and CA certificates (agent turns use git worktrees). Agent CLIs
(claude, codex, ...) are not included; build a derived image with the ones you
use.

## Trying it by hand (without the chart)

Use the chart in [`../helm/ligus`](../helm/ligus); it sets all of this, plus
the security context and NetworkPolicies. The minimum for one workspace is a
relay you can reach and one `ligus serve` StatefulSet. A sketch of the serve pod's settings; the full example, with the
security context and NetworkPolicies, is in the design document:

```yaml
env:
  - { name: LIGUS_DATA_DIR, value: /var/lib/ligus }
  # The pod network namespace is private to the pod; a Service and
  # NetworkPolicies decide who reaches the port. Never with hostNetwork.
  - { name: LIGUS_SERVE_LISTEN, value: "0.0.0.0:8787" }
  - { name: LIGUS_SERVE_ALLOW_ANY_BIND, value: "1" }
  - { name: LIGUS_SERVE_LABEL, value: k8s-serve }
  - { name: LIGUS_RELAY_URL, value: "https://relay.example.com" }
  - { name: LIGUS_WORKSPACE, valueFrom: { secretKeyRef: { name: ligus-workspace, key: room } } }
  # The key is a file, never env: the Secret is mounted with key -> cli.key
  # at this path for Ligus's k8s secret store (or set LIGUS_WORKSPACE_KEY_FILE).
  - { name: LIGUS_SECRET_BACKEND, value: k8s }
  - { name: LIGUS_SECRETS_DIR, value: /etc/ligus/secrets/store }
  # Fail closed without an agent sandbox (Landlock in a restricted pod).
  - { name: LIGUS_REQUIRE_SANDBOX, value: "1" }
  - { name: LIGUS_SANDBOX, value: landlock }
```

Then, from your own terminal (never in a Job whose logs are kept):

```sh
kubectl -n ligus-ws-acme exec -it ligus-serve-0 -- ligus pair --relay-only
```

(`ligus serve enroll`, which made the pod one member's device, is being
replaced in 2.0 by serve joining as its own member; see the operator guide.)

`ligus pair` prints a secret. Prefer the relay tunnel (`ligus remote enable`)
for the phone over exposing the serve API through an Ingress.

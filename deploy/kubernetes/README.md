# Ligus on Kubernetes

Status: **plan, not a supported install yet.** The design is in
[`docs/design/kubernetes.md`](../../docs/design/kubernetes.md). This directory
will hold the Helm chart and plain manifests as the phases in that document
land. Today it holds only this README; the container image for the `ligus`
binary is in [`../container/Dockerfile`](../container/Dockerfile).

## What runs where

| Component | Kubernetes shape | State | Holds E2EE keys? |
|---|---|---|---|
| Relay (separate repo, `ligus-ai/relay`) | Deployment + Postgres (external or an in-cluster operator); the tunnel part pinned to one replica | Postgres | **No**, ciphertext only |
| `ligus serve` (one per workspace) | StatefulSet, 1 replica, PVC for `LIGUS_DATA_DIR` | PVC (identity, `hive.db`, tunnel keys, checkouts) | Yes |
| `ligus worker` (today) | StatefulSet, one identity per replica, agents sharded across replicas by host binding | PVC per replica | Yes |
| Turn executors (planned) | Job or agent-sandbox `Sandbox` per turn, hardened, optional gVisor/Kata | none (ephemeral) | **No** |
| MCP servers | Deployment + Service (or ToolHive / kagent CRDs); stdio servers run inside the turn | per server | No |
| Local models | vLLM / KServe / llm-d behind an OpenAI-compatible Service | model cache | No |

Two rules follow from Ligus's design and shape everything here:

1. **One identity, one writer.** A `ligus serve` or `ligus worker` process is a
   Ligus *device*: its keys and its SQLite event log live in its data dir. Never
   run two pods on the same data dir, and never put `hive.db` on a network
   filesystem (NFS, SMB, RWX volumes). Use a `ReadWriteOnce` PVC and a
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

## Trying it by hand (until the chart exists)

The minimum for one workspace is a relay you can reach and one `ligus serve`
StatefulSet. A sketch of the serve pod's settings; the full example, with the
security context and NetworkPolicies, is in the design document:

```yaml
env:
  - { name: LIGUS_DATA_DIR, value: /var/lib/ligus }
  # The pod network namespace is private to the pod; a Service and
  # NetworkPolicies decide who reaches the port. Never with hostNetwork.
  - { name: LIGUS_SERVE_LISTEN, value: "0.0.0.0:8787" }
  - { name: LIGUS_SERVE_ALLOW_ANY_BIND, value: "1" }
  - { name: LIGUS_SERVE_LABEL, value: k8s-serve }
  - { name: LIGUS_RELAY_URL, valueFrom: { secretKeyRef: { name: ligus-workspace, key: relay-url } } }
  - { name: LIGUS_WORKSPACE, valueFrom: { secretKeyRef: { name: ligus-workspace, key: room } } }
  - { name: LIGUS_WORKSPACE_KEY, valueFrom: { secretKeyRef: { name: ligus-workspace, key: key } } }
```

Then, from your own terminal (never in a Job whose logs are kept):

```sh
kubectl -n ligus-ws-acme exec -it ligus-serve-0 -- ligus serve enroll   # optional: make it your device
kubectl -n ligus-ws-acme exec -it ligus-serve-0 -- ligus pair --relay-only
```

`ligus pair` prints a secret. Prefer the relay tunnel (`ligus remote enable`)
for the phone over exposing the serve API through an Ingress.

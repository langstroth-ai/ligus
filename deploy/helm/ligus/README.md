# ligus Helm chart (preview)

> **Preview.** Neither this chart nor the `ligus-relay` chart is published to a
> chart registry, and no container images are published: build them yourself
> (below). Values may still change before they are.

Runs the server side of one Ligus workspace on Kubernetes: an always-on
`ligus serve` device and, optionally, named `ligus worker` hosts and HTTP MCP
servers. The design and its reasoning are in
[`docs/design/kubernetes.md`](../../../docs/design/kubernetes.md); the
operator guide (supported nodes, install, backups, upgrades, what 2.0 does
not support) is the docs site's
[Kubernetes page](../../../docs-site/deploy/kubernetes.md).

**One release per workspace, in that workspace's own namespace.** The relay is
shared by many workspaces and normally lives in its own namespace and release
(the `ligus-relay` chart in the relay repository, v0.3.0); this chart can also
pull it in as a dependency for an all-in-one install.

Requires a CLI image of 2.0 or later.

## What it renders

| Component | Kind | Notes |
|---|---|---|
| serve | StatefulSet, `replicas: 1`, PVCs `data` (`/var/lib/ligus`) and `home` (`/home/ligus`) | Identity, device keys, the event log, tunnel keys, checkouts. One identity, one writer: never scale it, never share the volume, never put it on NFS/RWX |
| serve Service | ClusterIP 8787 | Health, phone API. Reach it from the phone through the relay tunnel, never a public Ingress |
| workers | One StatefulSet (one replica) per `worker.instances[]` entry | Optional. Each is its own host (`--label <name>`); bind agents with `ligus set-agent-host` |
| MCP servers | Deployment + Service + NetworkPolicy each | Optional HTTP MCP servers, reachable only from this workspace's serve and workers |
| NetworkPolicies | serve/workers, tests, MCP servers, optional namespace default-deny | Egress closed by default |
| relay | `ligus-relay` dependency | Optional, `relay.enabled` |

Every pod runs as UID 65532 with a read-only root filesystem, no capabilities,
`RuntimeDefault` seccomp, no service account token and no `hostNetwork`, so the
namespace can enforce Pod Security `restricted`.

## The agent sandbox

serve and workers run agent processes (CLI agents, `run_command`, stdio MCP
servers) in their own pod, confined by Ligus's sandbox. In a restricted pod
that is Landlock (Linux 6.7+ with `landlock` in the node's LSM list; the deploy
docs have the node matrix). With no confinement Ligus refuses every agent turn
that runs a local program, so the chart fails closed:

- `LIGUS_REQUIRE_SANDBOX=1` (`agentSandbox.require`): serve and workers exit
  instead of starting unconfined.
- `LIGUS_SANDBOX=landlock` (`agentSandbox.tier`).
- A `sandbox-preflight` init container runs
  `ligus doctor --require-confined --expect-tier landlock --turn-test`: one real
  agent turn against an in-process stub model whose `run_command` must run and
  must not read `/proc/1/environ`, the data dir or the secret files. On a node
  that can't do it the pod stays in `Init` and is never Ready.
- `helm test` runs the same turn (`test-sandbox`).

## Install

Build the images (the relay from a relay checkout at v0.3.0):

```sh
docker build -f deploy/container/Dockerfile -t registry.example.com/ligus:2.0.0 .
```

The chart's only dependency is the relay chart, which is optional but must be
present for Helm to render. Until it is published, package it from a relay
checkout at v0.3.0:

```sh
mkdir -p deploy/helm/ligus/charts
helm package -d deploy/helm/ligus/charts <relay-checkout>/deploy/helm/ligus-relay
```

Create the workspace connection Secret from a desktop invite (the chart
generates no Secrets, so `helm template`, Argo CD and Flux render the same
thing every time). Keys: `room`, `key` and, if your relay requires one,
`relay-access-token` (then set `workspace.relayAccessToken=true`):

```sh
kubectl create namespace ligus-ws-acme
kubectl label namespace ligus-ws-acme pod-security.kubernetes.io/enforce=restricted
kubectl -n ligus-ws-acme create secret generic acme-workspace \
  --from-literal=room=<room> --from-literal=key=<workspace key>
helm install acme deploy/helm/ligus -n ligus-ws-acme \
  --set image.repository=registry.example.com/ligus --set image.tag=2.0.0 \
  --set relayUrl=https://relay.example.com \
  --set networkPolicy.allowInternetHttps=true \
  --set workspace.existingSecret=acme-workspace
helm test acme -n ligus-ws-acme --logs
```

`networkPolicy.allowInternetHttps=true` lets serve reach a relay (and hosted
model APIs) outside the cluster over HTTPS; for a relay in the cluster set
`networkPolicy.relayNamespace=<its namespace>` instead.

A throwaway test workspace (nobody else holds the key; CI only):

```sh
kubectl -n <ns> create secret generic ws-test \
  --from-literal=room=k8s-test-$(uuidgen | tr A-Z a-z) \
  --from-literal=key=$(openssl rand -base64 36)
```

Then, from your own terminal (it prints a secret; never run it as a Job, whose
logs are kept):

```sh
kubectl -n ligus-ws-acme exec -it acme-ligus-serve-0 -- ligus pair --relay-only   # needs remote.enabled
```

**Changing in 2.0:** `ligus serve enroll` and `serve.owner` make the box act
for an owner. They are being replaced by serve joining the workspace as its
own member (a service member an Admin approves, capped below Admin). Until
that lands they still work, but don't build automation on them.

## Secrets

Ligus's own secrets are files, never environment variables. They are mounted
`0400` (with `fsGroup` the kubelet adds group read, so they end up `0440`
`root:65532`) under `/etc/ligus/secrets`, passed as `LIGUS_*_FILE`, and denied
to agent processes:

| Secret | Value | Variable |
|---|---|---|
| workspace key (and relay access token) | `workspace.existingSecret` | `LIGUS_WORKSPACE_KEY_FILE`, `LIGUS_RELAY_ACCESS_TOKEN_FILE` |
| workspace runtime credentials (one key per `secret_ref`) | `serve.workspaceSecrets.existingSecret`, `worker.workspaceSecrets.existingSecret` | `LIGUS_WS_SECRET_DIR` |
| tunnel registration token | `remote.regTokenSecret` | `LIGUS_REMOTE_REG_TOKEN_FILE` |
| ntfy token | `serve.ntfy.tokenSecret` | `LIGUS_NTFY_TOKEN_FILE` |

Provider keys that CLI agents read from their environment
(`ANTHROPIC_API_KEY`, ...) go in `serve.agentEnv` / `worker.agentEnv`, one
`{name, secretName, secretKey}` each; every name must be in
`agentEnvAllowList`. These do reach agent processes, which need them: use keys
you can rotate and limit. `envFrom` of whole Secrets (the old
`serve.secretEnv`) is refused.

Pods carry a `checksum/secrets` annotation, so `helm upgrade` rolls them when a
referenced Secret changed. It uses `lookup`, which is a constant under `helm
template`; with GitOps use [Reloader](https://github.com/stakater/Reloader) or
bump `podAnnotations`.

## Values

| Key | Default | Notes |
|---|---|---|
| `image.repository` / `tag` / `digest` | `ghcr.io/ligus-ai/ligus`, appVersion | Built from `deploy/container/Dockerfile`; pin by digest |
| `relayUrl` | empty | `LIGUS_RELAY_URL`; required unless `relay.enabled` |
| `relay.enabled` | `false` | In-release relay (any `ligus-relay` value under `relay.`) |
| `workspace.existingSecret` / `secretKeys` / `relayAccessToken` | required | Workspace connection |
| `remote.enabled` / `relayUrl` / `insecureRelay` / `regTokenSecret` | off | Phone access through the relay tunnel; `insecureRelay` only for in-cluster http |
| `serve.label`, `name`, `owner`, `defaultAgent`, `applyOnApprove` | | `LIGUS_SERVE_*` (`owner` is changing in 2.0, above) |
| `serve.runtimesToml` | empty | Mounted read-only, `LIGUS_RUNTIMES_FILE` |
| `serve.mcpToml`, `serve.dashboardToml` | empty | Mounted read-only (`LIGUS_MCP_CONFIG`, `LIGUS_DASHBOARD_FILE`) |
| `serve.workspaceSecrets`, `serve.agentEnv` | empty | Secrets, above |
| `serve.ntfy.*` | off | Notifications |
| `serve.persistence.*` | 5Gi, `ReadWriteOncePod` | Use an encrypted StorageClass. RWOP needs a CSI driver; `ReadWriteOnce` allows two pods on one node |
| `serve.home.*` | 2Gi | `HOME`, its own volume |
| `worker.enabled` / `instances[]` / `permissionMode` | off, `[]` | Named worker hosts: `{name, permissionMode?, resources?}` |
| `agentSandbox.require` / `tier` / `preflight.*` | on, `landlock`, on | The agent sandbox, above |
| `mcp.servers[]` | `[]` | `name`, `image` (pinned by digest; a tag needs `allowUnpinned: true`), `args`, `port`, `env`, `internetHttps` (default on), `egress`, `resources` |
| `networkPolicy.restrictEgress` | `true` | serve/worker egress: DNS, the relay, MCP servers, plus what you allow |
| `networkPolicy.relayNamespace` | empty | A relay in this cluster |
| `networkPolicy.allowInternetHttps` / `internetExcept` | off, metadata + private ranges | 443 to the internet, never cloud metadata (169.254.0.0/16, 100.100.100.200) or private ranges |
| `networkPolicy.extraEgress` | `[]` | More NetworkPolicyEgressRules (an in-cluster model endpoint) |
| `networkPolicy.namespaceDefaultDeny` | `false` | Namespace-wide deny-all |
| `networkPolicy.serveIngressFrom` | this release's own pods | Who reaches serve:8787 |
| `networkPolicy.dns` | kube-system `k8s-app=kube-dns` | The only DNS peer any policy allows |

## Notes and known limits

- `ligus serve` binds `0.0.0.0:8787` inside the pod with
  `LIGUS_SERVE_ALLOW_ANY_BIND=1`. That is safe because the pod's network
  namespace is private and the chart never sets `hostNetwork`; the Service type
  and NetworkPolicy decide who connects.
- Keyless runtimes and built-in file tools accept in-cluster model endpoints
  only by their full `*.svc.cluster.local` name today.
- NetworkPolicy is programmed asynchronously for a new pod on some CNIs (kind's
  kindnet among them): a pod's first second or two of traffic can precede its
  policy.
- Changing `persistence.accessMode`, the `home` volume or the worker layout
  changes `volumeClaimTemplates`, which Kubernetes doesn't allow in place:
  delete the StatefulSet with `--cascade=orphan` first (the PVCs stay).
- Kubernetes per-turn executors, PR-test sandboxes, worker autoscaling and
  OpenTelemetry export are not supported in 2.0.

## CI

`.github/workflows/kind-e2e.yml` builds the CLI and relay images (the relay at a
pinned commit), installs the relay (Postgres) and a throwaway workspace with
one worker on kind, and checks: sync between two client identities through the
relay, a stub-model agent turn under Landlock in the serve container (tier,
`/proc/1/environ` denied, serve not dumpable), worker readiness, the tunnel
host connection, no plaintext in the relay database, restart on the same
identity, Pod Security `restricted`, and NetworkPolicy isolation of serve.
`ci/security_test.py` (needs `helm` and PyYAML) checks the rendered policy and
secret properties.

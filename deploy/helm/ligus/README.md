# ligus Helm chart

Runs the server side of one Ligus workspace on Kubernetes: an always-on
`ligus serve` device, optional sharded `ligus worker` hosts, optional MCP
servers and a hardened Job template for sandboxed turn executors. The design
and its reasoning are in
[`docs/design/kubernetes.md`](../../../docs/design/kubernetes.md).

**One release per workspace, in that workspace's own namespace.** The relay is
shared by many workspaces and normally lives in its own namespace and release
(the [`ligus-relay` chart](https://github.com/ligus-ai/relay/tree/main/deploy/helm/ligus-relay));
this chart can also pull it in as a dependency for an all-in-one install.

Requires a CLI image of v1.19 or later (the chart sets `LIGUS_*` variables).

## What it renders

| Component | Kind | Notes |
|---|---|---|
| serve | StatefulSet, `replicas: 1`, PVC at `/var/lib/ligus` | Identity, device keys, the event log, tunnel keys, checkouts. One identity, one writer: never scale it, never share the volume, never put it on NFS/RWX |
| serve Service | ClusterIP 8787 | Health, phone API. Reach it from the phone through the relay tunnel, not an Ingress |
| worker | StatefulSet, N replicas, PVC each | Optional. Each replica is its own host (`--label <pod name>`); bind agents with `ligus set-agent-host`. Sharding, not autoscaling |
| MCP servers | Deployment + Service + NetworkPolicy each | Optional HTTP MCP servers, reachable only from this workspace's pods |
| sandbox | ConfigMap with the hardened executor Job, executor NetworkPolicy, optional smoke Job | Template for the controller/executor split (Phase 3) and PR tests |
| relay | `ligus-relay` dependency | Optional, `relay.enabled` |

Every pod runs as UID 65532 with a read-only root filesystem, no capabilities,
`RuntimeDefault` seccomp, no service account token and no `hostNetwork`, so
the namespace can enforce Pod Security `restricted`.

## Install

The chart's only dependency is the relay chart, which is optional but must be
present for Helm to render. Until it is published as an OCI chart, package it
from a relay checkout:

```sh
mkdir -p deploy/helm/ligus/charts
helm package -d deploy/helm/ligus/charts <relay-checkout>/deploy/helm/ligus-relay
```

(Once published: `helm dependency build deploy/helm/ligus`.)

Create the workspace connection from a desktop invite. Keys: `room`, `key`
and, if your relay requires one, `relay-access-token`:

```sh
kubectl create namespace ligus-ws-acme
kubectl label namespace ligus-ws-acme pod-security.kubernetes.io/enforce=restricted
kubectl -n ligus-ws-acme create secret generic acme-workspace \
  --from-literal=room=<room> --from-literal=key=<workspace key>
helm install acme deploy/helm/ligus -n ligus-ws-acme \
  --set relayUrl=https://relay.example.com \
  --set workspace.existingSecret=acme-workspace
```

Then, from your own terminal (these print secrets; never run them as Jobs,
whose logs are kept):

```sh
kubectl -n ligus-ws-acme exec -it acme-ligus-serve-0 -- ligus serve enroll   # optional: make it your device
kubectl -n ligus-ws-acme exec -it acme-ligus-serve-0 -- ligus pair --relay-only
```

### Test mode

`workspace.generate=true` creates a throwaway workspace: a random room and key
in a Secret owned by the release (kept across upgrades and on uninstall, since
the serve PVC outlives the release). No desktop holds that key, so nothing
outside the cluster can read the workspace. Use it for CI and trials only.
`ci/kind-values.yaml` is the configuration the kind end-to-end test uses.

## Values

| Key | Default | Notes |
|---|---|---|
| `image.repository` / `tag` / `digest` | `ghcr.io/ligus-ai/ligus`, appVersion | Built from `deploy/container/Dockerfile` |
| `relayUrl` | empty | `LIGUS_RELAY_URL`; required unless `relay.enabled` |
| `relay.enabled` | `false` | In-release relay (any `ligus-relay` value under `relay.`) |
| `workspace.existingSecret` / `secretKeys` | empty | Workspace connection |
| `workspace.generate` | `false` | Throwaway test workspace |
| `remote.enabled` / `relayUrl` / `insecureRelay` / `regTokenSecret` | off | Phone access through the relay tunnel (`LIGUS_REMOTE_RELAY`); `insecureRelay` only for in-cluster http |
| `serve.label`, `name`, `owner`, `defaultAgent`, `applyOnApprove` | | `LIGUS_SERVE_*` |
| `serve.runtimesToml` | empty | Copied into the data dir by an init container on every start |
| `serve.mcpToml`, `serve.dashboardToml` | empty | Mounted read-only (`LIGUS_MCP_CONFIG`, `LIGUS_DASHBOARD_FILE`) |
| `serve.secretEnv.existingSecret` | empty | All keys become env: `LIGUS_WS_SECRET_*`, model keys, MCP tokens |
| `serve.ntfy.*` | off | Notifications |
| `serve.persistence.*` | 5Gi | Use an encrypted StorageClass: the volume is as sensitive as the workspace key |
| `worker.enabled` / `replicas` / `permissionMode` | off, 1 | Sharded hosts |
| `mcp.servers[]` | `[]` | `name`, `image`, `args`, `port`, `env`, `secretEnv`, `resources` |
| `sandbox.enabled` / `profile` | off, `agent-turn` | `pr-test` has no egress at all and refuses to render without `runtimeClassName` unless `allowDefaultRuntime` |
| `sandbox.runtimeClassName` | empty | `gvisor`, a Kata class, ...; empty means runc, which is not a security boundary |
| `sandbox.egress` | `[]` | Extra egress rules for agent-turn executors (model endpoint, granted MCP Services) |
| `sandbox.smokeJob.enabled` | `false` | Post-install Job that verifies the executor hardening |
| `networkPolicy.serveIngressFrom` | this release's own pods | Who reaches serve:8787; MCP server pods and other pods in the namespace are excluded by default |
| `networkPolicy.restrictEgress` / `relayNamespace` / `extraEgress` | off | Lock serve and worker egress down to DNS, the relay and listed peers |
| `networkPolicy.dns` | kube-system `k8s-app=kube-dns` | The only DNS peer any policy allows; change it if your cluster DNS is labelled differently |

## Notes and known limits

- `ligus serve` binds `0.0.0.0:8787` inside the pod with
  `LIGUS_SERVE_ALLOW_ANY_BIND=1`. That is safe because the pod's network
  namespace is private and the chart never sets `hostNetwork`; the Service type
  and NetworkPolicy decide who connects.
- `ligus worker` has no health endpoint yet, so worker pods have no probes.
- Keyless runtimes and built-in file tools accept in-cluster model endpoints
  only by their full `*.svc.cluster.local` name today.
- NetworkPolicy is programmed asynchronously for a new pod on some CNIs (kind's
  kindnet among them): a pod's first second or two of traffic can precede its
  policy. Executors must not start untrusted work immediately, and the smoke
  Job waits before checking. On kindnet, egress to the `kubernetes` Service VIP
  was also observed to bypass an egress policy in some runs; executors have no
  service account token, so only anonymous API access is possible.
- The sandbox Job template expects the controller to add the turn bundle
  volume (`/in`); `ligus exec-turn` is Phase 3 work.

## CI

`.github/workflows/kind-e2e.yml` builds the CLI and relay images, installs the
relay (Postgres) and a throwaway workspace on kind, and checks: sync between
two client identities through the relay, the tunnel host connection, no
plaintext in the relay database, restart on the same identity, Pod Security
`restricted`, NetworkPolicy isolation of serve, and the sandbox hardening Job.

# ligus Helm chart (preview)

> **Preview.** Neither this chart nor the `langstroth-relay` chart is published to a
> chart registry, and no container images are published: build them yourself
> (below). Values may still change before they are.

Runs the server side of one Ligus workspace on Kubernetes: an always-on
`ligus serve` device and, optionally, named `ligus worker` hosts and HTTP MCP
servers. The design and its reasoning are in
[`docs/design/kubernetes.md`](../../../docs/design/kubernetes.md); the
operator guide (supported nodes, install, backups, upgrades, what 0.1 does
not support) is the docs site's
[Kubernetes page](../../../docs-site/docs/operate/deploy/kubernetes.md).

**One release per workspace, in that workspace's own namespace.** The relay is
shared by many workspaces and normally lives in its own namespace and release
(the `langstroth-relay` chart in the relay repository, v0.3.0); this chart can also
pull it in as a dependency for an all-in-one install.

Requires a CLI image of 0.1 or later.

## What it renders

| Component | Kind | Notes |
|---|---|---|
| serve | StatefulSet, `replicas: 1`, PVCs `data` (`/var/lib/ligus`) and `home` (`/home/ligus`) | Identity, device keys, the event log, tunnel keys, checkouts. One identity, one writer: never scale it, never share the volume, never put it on NFS/RWX |
| serve Service | ClusterIP 8787 | Health, phone API. Reach it from the phone through the relay tunnel, never a public Ingress |
| workers | One StatefulSet (one replica) per `worker.instances[]` entry | Optional. Each is its own host (`--label <name>`); bind agents with `ligus set-agent-host` |
| MCP servers | Deployment + Service + NetworkPolicy each | Optional HTTP MCP servers, reachable only from this workspace's serve and workers |
| NetworkPolicies | serve/workers, tests, MCP servers, optional namespace default-deny | Egress closed by default |
| relay | `langstroth-relay` dependency | Optional, `relay.enabled` |

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
- `helm test` runs the same turn (`test-sandbox`). Test pods never hold the
  workspace key: `test-sync` checks sync through the relay on a throwaway
  workspace it makes up (`tests.sync.enabled=false` for relays that admit only
  workspace-scoped tokens).

## Install

This chart and the `ligus` image are not published. Build the image and push
it to a registry your cluster can pull from (the relay image,
`ghcr.io/langstroth-ai/langstroth-relay:0.3.0`, is published):

```sh
docker build -f deploy/container/Dockerfile -t registry.example.com/ligus:0.4.0 .
docker push registry.example.com/ligus:0.4.0
```

The chart's only dependency is the relay chart, which is optional but must be
present for Helm to render. It is published at
`oci://ghcr.io/langstroth-ai/charts/langstroth-relay` (0.3.0); fetch it into
`charts/` (no registry login needed):

```sh
helm dependency build deploy/helm/ligus
```

Without network access to ghcr.io, package it from a relay checkout at
v0.3.0 instead: `helm package -d deploy/helm/ligus/charts
<relay-checkout>/deploy/helm/langstroth-relay`.

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
  --set image.repository=registry.example.com/ligus --set image.tag=0.4.0 \
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

## Admitting serve and workers

serve and every worker join the workspace as their own **service member**:
at most Contributor, their votes never count as a person's approval, and
they admit, remove and revoke no one. An Admin admits each box once, with a
passkey; nobody needs to be at the pod.

1. On the desktop, right-click the workspace: **Admit a box… → For
   Kubernetes**. Pick how long the code stays redeemable (24 h by default, at
   most 6 days) and confirm with your passkey. One code per box: each is
   single use. A headless Admin can use `ligus admit issue
   --recovery-words-file …` instead.
2. Store each code in its own Secret:

    ```sh
    kubectl -n ligus-ws-acme create secret generic acme-serve-admission --from-file=admission=./serve.txt
    kubectl -n ligus-ws-acme create secret generic acme-build-1-admission --from-file=admission=./build-1.txt
    ```

3. Name them:

    ```yaml
    serve:
      admission: { existingSecret: acme-serve-admission }
    worker:
      enabled: true
      instances:
        - name: build-1
          admission: { existingSecret: acme-build-1-admission }
    ```

At start-up each box syncs first and refuses to run without a copy of the
workspace (a box never founds one or names itself Owner), then redeems its
code from `LIGUS_ADMISSION_FILE` (mounted `0400`, read-only, optional,
denied to agent processes, never seen by the sandbox preflight). Redeeming
binds the admission to that box's key. **Delete the Secret once the box has
joined** (its log says `joined workspace '…' as its own service member`); a
restart doesn't need it again.

A bearer admission code is a secret until it is redeemed (it also carries the
workspace key): never put it in a values file. `serve.owner` and `ligus
serve enroll` (the box acting as a person's device) were removed in 0.1; the
chart refuses `serve.owner` with a migration error.

## Secrets

Ligus's own secrets are files, never environment variables. They are mounted
`0400` (with `fsGroup` the kubelet adds group read, so they end up `0440`
`root:65532`) under `/etc/ligus/secrets`, and denied to agent processes. The
workspace key goes through Ligus's k8s secret-store backend, so no environment
variable overrides it on restart; the rest are `LIGUS_*_FILE` variables
(the CLI also accepts `LIGUS_WORKSPACE_KEY_FILE` and
`LIGUS_RELAY_ACCESS_TOKEN_FILE` outside the chart):

| Secret | Value | Variable |
|---|---|---|
| workspace key (and relay access token) | `workspace.existingSecret` | Ligus's k8s secret store: `LIGUS_SECRET_BACKEND=k8s`, mounted as `cli.key` / `cli.token` in `LIGUS_SECRETS_DIR` |
| workspace runtime credentials (one key per `secret_ref`) | `serve.workspaceSecrets.existingSecret`, `worker.workspaceSecrets.existingSecret` | `LIGUS_WS_SECRET_DIR` |
| tunnel registration token | `remote.regTokenSecret` | `LIGUS_REMOTE_REG_TOKEN_FILE` |
| ntfy token | `serve.ntfy.tokenSecret` | `LIGUS_NTFY_TOKEN_FILE` |
| box admission (one per box, delete after it joined) | `serve.admission.existingSecret`, `worker.instances[].admission.existingSecret` | `LIGUS_ADMISSION_FILE` |

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

## Model gateways

serve and workers can route every agent's model traffic through a dedicated
LLM gateway: LiteLLM, kgateway or Envoy AI Gateway (Envoy and the Gateway
API), vLLM or KServe pools behind the Gateway API Inference Extension, or any
other OpenAI- or Anthropic-compatible endpoint. None is required and none is
bundled. Design: `docs/design/v2/model-gateways.md`; operator guide:
[Deploying with an LLM gateway](https://docs.langstroth.ai/operate/deploy/model-gateways/). A full example is in
[`examples/model-gateway-values.yaml`](examples/model-gateway-values.yaml).

```yaml
modelGateways:
  - name: team-gw                       # what the workspace sees (names only)
    url: https://llm-gateway.gw.svc.cluster.local:8443
    api: openai                         # openai | anthropic
    auth:
      mode: serviceAccountToken         # serviceAccountToken | apiKey | mtls | none
      audience: llm-gateway
      expirationSeconds: 3600
    networkPolicy:
      namespaceSelector: { matchLabels: { kubernetes.io/metadata.name: gw } }
      podSelector: { matchLabels: { app.kubernetes.io/name: llm-gateway } }
      ports: [8443]
```

What the chart renders for a non-empty `modelGateways`:

- **The allow-list file.** ConfigMap `<release>-model-gateways`
  (`model-gateways.json`: name, URL, wire, auth mode and the paths of the
  credential files; no secrets), mounted read-only and named by
  `LIGUS_MODEL_GATEWAYS_FILE`. The host advertises the gateway names (never
  URLs or credentials) to the workspace; an Admin picks one for a runtime,
  with passkey presence. With `modelGatewaysExclusive: true` (the default)
  every agent turn on these hosts must go through a listed gateway, and a
  runtime pointing anywhere else is refused.
- **Credentials as files, one projected volume per gateway**, under
  `/etc/ligus/model-gateways/<name>/`, never in the environment:
  - `serviceAccountToken`: a projected ServiceAccount token with the given
    `audience` (default: the gateway name; each gateway's must differ and
    never be the API server's) and `expirationSeconds` (default 3600,
    600 to 86400). The pods keep `automountServiceAccountToken: false`,
    so this audience-bound token is the only one mounted. The kubelet rotates
    it and Ligus re-reads it on every request.
  - `apiKey`: `auth.secretName` / `auth.secretKey`, sent as
    `Authorization: Bearer` or the header in `auth.header`
    (`x-api-key`, `api-key`).
  - `mtls`: `auth.secretName`, a `kubernetes.io/tls` Secret with `tls.crt`,
    `tls.key` and `ca.crt` (cert-manager's shape); the CA is pinned.
  - `caSecret: {name, key}` pins a private CA for any https gateway.
  Agent processes never see these files (Ligus keeps them off its sandbox's
  read list) and never get the credential: the egress proxy adds it to their
  requests.
- **NetworkPolicy `<release>-model-gateways`**: egress from serve and
  workers to exactly each gateway's `namespaceSelector`/`podSelector` or
  `ipBlock.cidrs`, on its `ports`. Policies are a union, so this only adds to
  `<release>-controller`, and it renders only with
  `networkPolicy.restrictEgress` (without it, egress isn't restricted). A
  gateway in another cluster is an `ipBlock` of its load balancer's
  addresses.
- `modelGatewayAttribution.secretName`: the key for the opt-in
  `attribution` headers (`X-Ligus-Workspace`, `X-Ligus-Agent`: keyed hashes,
  no names or content). Use one Secret for serve and every worker so the ids
  agree across hosts.

The chart refuses: names outside `[a-z0-9-]` (1-60), duplicates, an
`http://` URL without `allowPlaintext` (only inside a mesh with mTLS), a URL
with `user:pass@`, `mtls` over `http://`, an unknown `api` or `auth.mode`,
`apiKey`/`mtls` without a Secret, an SA token shorter than 600 s, a gateway
with no peers or no ports, an `ipBlock` broader than /8 (IPv4) or /16
(IPv6), and any CIDR that overlaps link-local or cloud metadata
(169.254.0.0/16, 100.100.100.200/32, fe80::/10, fd00:ec2::254/128). A
private-range CIDR is allowed, but only exactly as listed. `extraEnv` can't
set `LIGUS_MODEL_GATEWAY*`.

## Values

| Key | Default | Notes |
|---|---|---|
| `image.repository` / `tag` / `digest` | `ghcr.io/langstroth-ai/ligus`, appVersion | Built from `deploy/container/Dockerfile`; pin by digest |
| `relayUrl` | empty | `LIGUS_RELAY_URL`; required unless `relay.enabled` |
| `relay.enabled` | `false` | In-release relay (any `langstroth-relay` value under `relay.`) |
| `workspace.existingSecret` / `secretKeys` / `relayAccessToken` | required | Workspace connection |
| `remote.enabled` / `relayUrl` / `insecureRelay` / `regTokenSecret` | off | Phone access through the relay tunnel; `insecureRelay` only for in-cluster http |
| `serve.label`, `name`, `defaultAgent` | | `LIGUS_SERVE_*` (there is no auto-apply: a box applies a proposal only when a person asks it to, "Apply on ‹host›") |
| `serve.admission.existingSecret` / `secretKey` | empty, `admission` | serve's box admission Secret (above) |
| `serve.runtimesToml` | empty | Mounted read-only, `LIGUS_RUNTIMES_FILE` |
| `serve.mcpToml`, `serve.dashboardToml` | empty | Mounted read-only (`LIGUS_MCP_CONFIG`, `LIGUS_DASHBOARD_FILE`) |
| `serve.workspaceSecrets`, `serve.agentEnv` | empty | Secrets, above |
| `serve.ntfy.*` | off | Notifications: `url` must be `https://` unless `allowHttp: true`; generic push text unless `titles: true` |
| `serve.persistence.*` | 5Gi, `ReadWriteOncePod` | Use an encrypted StorageClass. RWOP needs a CSI driver; `ReadWriteOnce` allows two pods on one node |
| `serve.home.*` | 2Gi | `HOME`, its own volume |
| `worker.enabled` / `instances[]` / `permissionMode` | off, `[]` | Named worker hosts: `{name, admission?: {existingSecret, secretKey?}, permissionMode?, resources?}`; each needs its own admission |
| `agentSandbox.require` / `tier` / `preflight.*` | on, `landlock`, on | The agent sandbox, above |
| `mcp.servers[]` | `[]` | `name`, `image` (pinned by digest; a tag needs `allowUnpinned: true`), `args`, `port`, `env`, `internetHttps` (default on), `egress`, `resources`, `podSecurityContext` / `securityContext` (replace the chart's UID 65532 and read-only root for images that need it) |
| `modelGateways[]` | `[]` | LLM gateways serve and workers may use: `name`, `url`, `api`, `auth`, `caSecret`, `allowPlaintext`, `attribution`, `networkPolicy` (Model gateways, above) |
| `modelGatewaysExclusive` | `true` | With gateways listed, every agent turn must use one (`LIGUS_MODEL_GATEWAYS_EXCLUSIVE`) |
| `modelGatewayAttribution.secretName` / `secretKey` | empty, `key` | HMAC key for the opt-in attribution headers |
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
  OpenTelemetry export are not supported in 0.1.

## CI

`.github/workflows/kind-e2e.yml` builds the CLI and relay images (the relay at a
pinned commit), installs the relay (Postgres), a throwaway workspace served by
serve, and one worker on a second throwaway workspace of its own on kind (0.1
admits a box to a workspace only through an Admin's invite, which CI cannot
issue headlessly yet, so serve and a worker can't share a fresh workspace), and
checks: the runner kernel has Landlock (the job fails, saying so, if not), sync between two client identities through the
relay, a stub-model agent turn under Landlock in the serve container (tier,
`/proc/1/environ` denied, serve not dumpable), worker readiness, the tunnel
host connection, no plaintext in the relay database, restart on the same
identity, Pod Security `restricted`, and NetworkPolicy isolation of serve.
`ci/security_test.py` (needs `helm` and PyYAML) checks the rendered policy and
secret properties.

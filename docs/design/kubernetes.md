# Kubernetes as a deployment option

> **2.0 status (2026-11-16).** What ships in 2.0 is the preview Helm charts
> described in `docs-site/deploy/kubernetes.md`: serve and named workers run
> agent turns in their own pods under Landlock (fail-closed preflight),
> secrets as files, closed egress, single replicas. The executor/controller
> split, PR-test sandboxes, OTLP export, keys off the cluster, multi-tenant
> clusters and relay/auth HA below are **not** in 2.0 (see that page's
> "Explicitly unsupported" list). Serve enrollment as an owner's device
> (`serve enroll`, `LIGUS_SERVE_OWNER`) is being replaced by serve joining as
> its own member. This document is the longer design and keeps the older
> Hive names.

Status: **design, not built.** Phase 0 (this document, a container image for
the `hive` binary and `deploy/kubernetes/README.md`) is in the tree. Everything
else is a proposal with acceptance tests per phase. Claims about third-party
projects were checked against their public docs in November 2026 and are cited
inline; anything not confirmed is marked *(unverified)*.

## 1. Goal and non-goals

**Goal.** A team (or a hosting provider) can run every server-side piece of
Hive on a Kubernetes cluster: the relay, an always-on `hive serve` per
workspace, agent runners that scale with work, MCP tool servers and in-cluster
model inference, with each agent turn able to run in an isolated sandbox. The
same chart should work on a laptop (kind or k3d) and on a managed cluster.

**Non-goals.**

- The desktop app and the iPhone app stay clients. Nothing here moves the
  workspace's source of truth into the cluster: Hive state is still a
  per-device, end-to-end encrypted event log, and the cluster's pods are
  devices like any other.
- No change to the relay's wire contract (`/v1`, `hrt1`, `hive-tunnel/1`).
- No operator in the first phases (see [Section 10](#10-packaging)).

## 2. What Hive's design implies for Kubernetes

Four properties of today's code decide most of the shapes below.

1. **A headless process is a device.** `hive serve` and `hive worker` bootstrap
   an identity (account and device keys) in `LIGUS_DATA_DIR` on first run and
   keep their event log in `LIGUS_DATA_DIR/hive.db` (SQLite). The host id other
   members see is that device id (`cmd_register_worker` publishes
   `svc.device_id()`). Two pods sharing a data dir would be one device writing
   from two places; two pods with fresh data dirs are two different hosts.
   The docs already forbid SQLite on a network filesystem. So: **StatefulSet
   with a `ReadWriteOnce` PVC per identity, never a Deployment with replicas.**
2. **Agents are bound to exactly one host.** An agent's `host_id` names one
   device, and "once an agent is sent to a host, only that host answers its
   mentions" (`docs/ios-app.md`, Limits). Two worker pods are two hosts, so
   adding replicas does not add capacity for an agent; it only lets you shard
   agents across pods. Turn claims across devices are first-claim-wins in
   canonical fold order (`SessionEvent::TurnClaimed`), which is eventually
   consistent: two devices can both start a turn before they learn who won.
   A worker answers one turn at a time on one thread.
3. **Keys live where turns run.** Today the process that runs an agent turn
   also holds the workspace E2EE key, the device signing key, the git
   credentials and the model credentials. A turn's tools (file tools,
   `run_command`, CLI agents in edit modes, stdio MCP servers) execute in that
   same process tree. Isolation today is a per-turn git worktree, not a
   security boundary.
4. **Some relay state is in-process.** The relay stores envelopes, members,
   presence, keyring and tunnel registrations in Postgres when `DATABASE_URL`
   is set, but keeps live tunnel control connections, SSE fan-out, short
   pairing codes and rate limiters in memory. Its README says so: "Live
   control connections are kept in process: run a single relay instance for
   the tunnel", and SSE fan-out "is single-instance / in-process today".

The design below works with (1), proposes a change for (2) and (3) that turns
them into the scaling and the security model at once, and isolates (4).

## 3. Architecture

```
                    namespace: hive-relay                 namespace: hive-ws-<workspace> (one per workspace)
                 ┌─────────────────────────────┐        ┌───────────────────────────────────────────────────┐
 desktops ──────▶│ relay-sync  (Deployment, N) │◀──────▶│ hive-serve-0  (StatefulSet, 1)                    │
 phones   ──────▶│ relay-tunnel (Deployment, 1)│◀──────▶│   sync, API/SSE, tunnel host, turn controller     │
 (HTTPS)         │ Postgres (external or CNPG) │  sync  │   PVC: identity, hive.db, tunnel keys, checkouts  │
                 └─────────────────────────────┘        │   Secret: workspace key, relay token, git, models │
                     ciphertext only, no keys           │        │ creates (Kueue-admitted)                 │
                                                        │        ▼                                          │
                                                        │ turn executors (Job or Sandbox per turn)          │
                                                        │   no workspace key, no device key, no SA token    │
                                                        │   input: turn bundle + git bundle; output: reply  │
                                                        │   + patch; egress: model endpoint, allowed MCP    │
                                                        │                                                   │
                                                        │ mcp-<name> (Deployment + Service), optional       │
                                                        └───────────────────────────────────────────────────┘
                    namespace: hive-models (shared, optional)
                 ┌───────────────────────────────────────────────────────────────┐
                 │ vLLM / KServe LLMInferenceService / llm-d InferencePool       │
                 │ OpenAI-compatible Service, GPU nodes, scaled on queue depth   │
                 └───────────────────────────────────────────────────────────────┘
```

### 3.1 Relay

The relay is the one component that is already cloud-shaped: a static Go
binary, configured by environment variables, content-blind, with a health
endpoint that round-trips the store (`GET /v1/health`, `503` if the backend is
down) and a Prometheus-style `/metrics` endpoint (optionally gated by
`LIGUS_RELAY_METRICS_TOKEN`). It lives in its own repository
(`ligus-ai/relay`), and its chart should live there too and be pulled into
the Hive umbrella chart as a dependency.

**Store.** Always Postgres on Kubernetes (`DATABASE_URL` from a Secret, plus
`LIGUS_RELAY_REQUIRE_DB=1` so a misconfigured pod fails instead of silently
using the in-memory store). Either an external managed database (Cloud SQL,
RDS, Azure Database for PostgreSQL) or an in-cluster operator such as
CloudNativePG. The memory+snapshot store with a PVC is acceptable only for
kind/k3d.

**Split the deployment by what is in-process.** Run the same image twice:

| Deployment | Replicas | Routes | Settings |
|---|---|---|---|
| `relay-sync` | 1 to N (HPA on CPU or requests) | everything except `/v1/tunnel/*` | `LIGUS_RELAY_TUNNEL=off` |
| `relay-tunnel` | **exactly 1**, `strategy: Recreate` | `/v1/tunnel/*` | `LIGUS_RELAY_TUNNEL=open` or `allowlist`, APNs settings |

Both point at the same database, so host registrations, revoked phones and
push tokens (stored through `TunnelStore`) are shared. `Recreate` matters: with
a rolling update, the old pod still holds a host's control socket while a
phone's `/v1/tunnel/connect` lands on the new pod and fails. On shutdown the
relay closes tunnel sockets with `4011 shutting_down` and hosts reconnect, so a
`Recreate` rollout costs a few seconds of tunnel downtime and nothing else.
The APNs key (`LIGUS_RELAY_APNS_KEY` or a mounted `.p8` via
`LIGUS_RELAY_APNS_KEY_FILE`) is only needed on `relay-tunnel` *(verify which
instance sends pushes before splitting; the push path starts on the host's
control connection, so it should be the tunnel instance)*.

Caveats of N > 1 for `relay-sync`, all in-process today:

- **SSE nudges** only fire on the instance that appended the envelope. Clients
  keep polling with `?after=`, so this costs latency (up to the 3 s poll), not
  correctness. Fix upstream with Postgres `LISTEN/NOTIFY` (already tracked in
  the relay README).
- **Short pairing codes** (`POST /v1/pair`, `GET /v1/pair/{code}`) live in a
  per-process map; a code created on one replica is unknown to another. Until
  they move to the store, either keep `relay-sync` at one replica or route
  `/v1/pair*` to a single replica.
- **Rate limits** are per process, so the effective limit is N times the
  configured one.

Recommendation: start with one `relay-sync` replica plus one `relay-tunnel`
replica. Both are tiny. Scale `relay-sync` only after pairing codes move to
the store.

**Ingress.** Clients require `https://`. Use Gateway API `HTTPRoute` (or the
platform's Ingress) with TLS from cert-manager. The tunnel uses long-lived
WebSockets: the relay pings every 20 s and closes after 60 s of silence, so the
proxy's idle timeout must exceed 60 s, and the `hive-tunnel.1` subprotocol
header must pass through. The relay must see the client IP for its per-IP
limits (`X-Forwarded-For` from a trusted proxy *(check how the relay derives
the client IP before relying on this)*).

**Pod.** The current relay image is Alpine and starts as root to `chown` the
snapshot dir, then drops to an unprivileged user. On Kubernetes with Postgres
there is no data dir: run the binary directly as a non-root UID with
`readOnlyRootFilesystem`, which passes Pod Security `restricted`. A distroless
static image (`CGO_ENABLED=0` already) is a small change in the relay repo.

### 3.2 `hive serve`, one per workspace

`hive serve` is the always-on device for a workspace: relay sync, the agent
worker, the phone API and SSE stream, ntfy notifications, the relay tunnel
host and the dashboard. It supports one workspace per process today.

**Shape.** A StatefulSet with `replicas: 1`, a `volumeClaimTemplate` mounted
at `LIGUS_DATA_DIR` (`/var/lib/hive`), `podManagementPolicy: OrderedReady`,
and `terminationGracePeriodSeconds` above serve's 15 s turn grace (30 s). One
StatefulSet per workspace, in that workspace's namespace. A StatefulSet, not a
Deployment, because a Deployment's rolling update can briefly run two pods on
one `ReadWriteOnce` volume (if both land on the same node), which would be two
writers on one identity and one SQLite file.

**What lives on the PVC.** Device and account keys, `hive.db`, `config.toml`,
`serve.token`, `serve-state.json`, `repo-bindings.toml`, `runtimes.toml`,
`tunnel/` (host signing key, Noise static key, paired phones) and, by default,
`repos/` with the checkouts and per-chat worktrees. The PVC is secret
material: use an encrypted StorageClass, and if you back it up (Velero,
volume snapshots), the backup is as sensitive as the workspace key.

**Configuration.** Everything `hive serve` needs is already settable by
environment variable, so no config file has to be written by hand:

| Setting | Source |
|---|---|
| `LIGUS_RELAY_URL`, `LIGUS_WORKSPACE` | ConfigMap or Secret |
| `LIGUS_WORKSPACE_KEY`, `LIGUS_RELAY_ACCESS_TOKEN` | Secret (`secretKeyRef`) |
| `LIGUS_WS_SECRET_<ref>` (workspace runtime keys) | Secret |
| MCP tokens named by `token_env` in `mcp.toml` | Secret |
| `LIGUS_MCP_CONFIG`, `runtimes.toml`, `dashboard.toml` | ConfigMap mounted read-only; `runtimes.toml` must currently sit in the data dir, so copy it in with an init container or add a `LIGUS_RUNTIMES_FILE` setting (see [Section 12](#12-code-changes-prioritized)) |
| `LIGUS_NTFY_URL`, `LIGUS_NTFY_TOPIC`, `LIGUS_NTFY_TOKEN` | ConfigMap and Secret |
| `LIGUS_SERVE_LABEL`, `LIGUS_SERVE_NAME`, `LIGUS_SERVE_OWNER` | values |

`hive enroll` is not needed: env overrides `config.toml`. APNs keys do **not**
belong here; push is signed on the relay (see the relay section).

**Listening address.** `hive serve` refuses unspecified and public binds
unless `LIGUS_SERVE_ALLOW_ANY_BIND=1`. That check protects a host whose
interfaces face a LAN or the internet. A pod's network namespace is private to
the pod, so on Kubernetes binding `0.0.0.0:8787` inside the pod with
`LIGUS_SERVE_ALLOW_ANY_BIND=1` is the right setting, as long as the pod does
not use `hostNetwork` (the chart must forbid it). Binding the pod IP from the
downward API also passes the check without the override, but breaks
`kubectl port-forward` (which connects to loopback). Who reaches the port is
then decided by the Service type (ClusterIP only by default) and
NetworkPolicies. A small code change makes this explicit (Section 12).

**Reaching it from the phone.** In order of preference:

1. **Relay tunnel** (`hive remote enable --relay https://<relay>`): outbound
   WebSocket only, Noise IK end to end, per-phone credentials and revocation.
   No inbound exposure at all. This is the default for Kubernetes.
2. **Tailscale Kubernetes operator** exposing the ClusterIP Service on the
   tailnet: matches the documented "tailnet only" model.
3. **Ingress/HTTPRoute with TLS.** Possible, but it puts a single bearer-token
   API on the public internet. Opt-in only, behind a flag that says so, with
   IP allowlisting where the platform supports it.

**Pairing and enrollment** are interactive and print secrets. Run them with
`kubectl exec` from the operator's own terminal, never as Jobs (their logs
would keep the token):

```sh
kubectl -n hive-ws-acme exec -it hive-serve-0 -- hive remote enable --relay https://relay.example.com
kubectl -n hive-ws-acme rollout restart statefulset/hive-serve
kubectl -n hive-ws-acme exec -it hive-serve-0 -- hive serve enroll     # approve on the desktop
kubectl -n hive-ws-acme exec -it hive-serve-0 -- hive pair --relay-only
```

Owner enrollment (`LIGUS_SERVE_OWNER`) works unchanged: the desktop signs a
certificate for the pod's device key; the pod never sees the owner's account
key.

**Probes.** `GET /v1/health` is unauthenticated and cheap: use it for
liveness and readiness (`httpGet` on port 8787). A fresh pod whose first relay
sync fails exits and is restarted, which is the right behavior under a
`startupProbe` with a generous threshold. Readiness should eventually mean
"caught up with the relay" (Section 12).

### 3.3 Agent runners

Today there are two ways to run agents off the desktop: `hive serve` runs a
worker thread for agents bound to it, and `hive worker` is the same worker
without the API. Both run one turn at a time, and both run the turn in their
own process tree.

**Step 1: works with today's code (Phase 1).** A `hive-worker` StatefulSet
with one identity per replica (`hive-worker-0`, `hive-worker-1`, ...), each
registered as its own host (`--label $(POD_NAME)`). Agents are assigned to a
specific replica with `hive set-agent-host` or from the desktop's Hosts list.
This is sharding, not autoscaling: adding a replica adds a host, and nothing
moves to it until an agent is bound to it. Removing a replica leaves its
agents bound to an offline host (the desktop then offers "Run on worker").
Use it for a few long-lived agents with different credentials, images or
node needs (a GPU-attached worker for a CLI agent, a worker with
`LIGUS_WORKER_PERMISSION_MODE=acceptEdits` in a stricter namespace).

KEDA or an HPA on this StatefulSet would be wrong: scaling it up adds idle
hosts, and scaling it down strands agents.

**Step 2: controller and executors (Phase 3, recommended).** Split a turn in
two:

- The **turn controller** stays in the stateful pod (`hive serve` or
  `hive worker`). It syncs, claims the turn (one device, so no cross-device
  claim race), resolves the agent's runtime, assembles the prompt and context
  exactly as today, prepares a **git bundle** of the chat's base commit, and
  hands a **turn bundle** to an executor. When the executor finishes, the
  controller verifies the result, signs the reply events, files the patch as
  a `FileDiff` proposal, and syncs. Only the controller holds the workspace
  key, the device keys, git credentials and the relay token.
- A **turn executor** is an ephemeral pod (a Job or an agent-sandbox
  `Sandbox`, Section 4) that runs `hive exec-turn` (a new subcommand): it
  reads the turn bundle, unpacks the git bundle into a scratch checkout, runs
  the model and tool loop (built-in file tools, `run_command`, stdio MCP
  servers, or a CLI agent such as Claude Code), streams deltas back, and
  returns the reply text plus a `git diff` of the scratch checkout. It gets a
  model credential scoped to this turn and nothing else.

Capacity is then the number of executors in flight, bounded per workspace by
a Kueue quota (Section 8), with no change to how agents are bound to hosts:
the agent is still bound to the controller's host id, and the controller fans
out. A worker StatefulSet stays useful for separation of duties, but a single
controller per workspace can drive many concurrent turns.

The protocol between controller and executor is local to the namespace and
carries plaintext (the turn's prompt and files), so it must be authenticated
and confined: the controller creates the executor with a one-time token in
the turn bundle, the executor connects back to the controller's Service
(NetworkPolicy allows exactly that path), and the controller accepts results
only for turns it dispatched, within the turn's deadline. An alternative with
no callback is a result volume, but streaming deltas to the phone needs the
callback.

**Per-turn Jobs without the split** (each turn as a Job running the full
`hive` binary with the data dir) does not work: the Job would need the PVC
(one writer) and the keys (no isolation gained).

### 3.4 Repository checkouts

| Option | How | Fits |
|---|---|---|
| **PVC per workspace** (today's model) | Checkouts under `LIGUS_REPOS_DIR` on the serve/worker PVC; per-chat worktrees and `hive/chat/<id>` branches persist there; `LIGUS_SERVE_APPLY_ON_APPROVE=1` commits approved diffs to the chat branch | Phase 1; the controller in Phase 3 |
| **Git bundle per turn** (recommended for executors) | Controller runs `git bundle create` for the chat's base commit (full or `--since` a shallow point), passes it in the turn bundle; executor clones from the bundle with no remote | Phase 3; executor needs no git credentials and no network to a forge |
| **Ephemeral clone per turn** | Executor clones from the forge (partial clone, `--filter=blob:none`) with a short-lived read-only token | Very large repos where a bundle is too big; executor then needs forge egress and a credential |

Git credentials stay with the controller. On Kubernetes, replace a
`gh auth login` on the box with a **GitHub App**: its private key in a Secret,
installation tokens (one hour, scoped to the installed repositories and the
permissions the App was granted) minted per operation. Clone and fetch need
`contents: read`; pushing chat branches and opening pull requests need
`contents: write` and `pull_requests: write`. A fine-grained personal access
token in a Secret is the simpler fallback. Hive must not log either.

Untrusted pull-request tests (the "Run PR tests" design on the
`feat/fork-repos` branch, `docs/design/pr-test-sandbox.md`) map directly onto
executors: materialize the PR with `git archive` on the controller, hand the
tree to a sandbox with no network, no credentials and no service account
token, collect the log and JUnit output as untrusted data (Section 4.4).

### 3.5 MCP servers

Hive reads MCP servers from `LIGUS_MCP_CONFIG` (`stdio` with `command`/`args`,
or `http`/`sse` with `url` and a `token_env`). On Kubernetes:

- **HTTP MCP servers** run as their own Deployment and ClusterIP Service per
  server (or shared across workspaces in a tools namespace when they hold no
  per-workspace secrets). Hive's config points at
  `http://mcp-linear.hive-ws-acme.svc.cluster.local/mcp`, and the bearer token
  comes from a Secret by the env name in `token_env`. Separate Deployments
  are better than sidecars: tokens for Linear or GitHub stay out of the serve
  pod and out of executors, a misbehaving server restarts alone, and a
  NetworkPolicy can say exactly which pods may call it.
- **stdio MCP servers** run as subprocesses of the turn. In Phase 3 that
  means inside the executor, which is where untrusted tool code belongs. To
  share one stdio server across turns, wrap it with a stdio-to-HTTP proxy and
  treat it as an HTTP server.
- **Operators.** ToolHive's Kubernetes operator (`MCPServer` CRD, runs stdio
  or HTTP servers behind a proxy that exposes streamable HTTP) and kagent's
  `MCPServer`/`RemoteMCPServer` resources (via kmcp) both produce exactly
  this shape. Hive does not need either; the chart should accept a list of
  plain URLs and let a cluster use whichever it has. See Section 5 for
  versions.

Hive's own MCP policy stays in Hive: the requester-role gate
(`LIGUS_MCP_MIN_ROLE`) and the exfiltration guard (MCP calls refused after a
turn reads project files) run in the controller or executor before any call
leaves the pod. NetworkPolicies add a second layer: an executor may reach only
the MCP Services its turn was granted.

### 3.6 In-cluster model inference

A local model on Kubernetes is an OpenAI-compatible HTTP endpoint, which
Hive already supports as a `custom` runtime:

```toml
# runtimes.toml on the serve/worker pod
[[runtime]]
id = "*"
name = "In-cluster vLLM"
provider = "custom"
model = "Qwen/Qwen3-32B"
endpoint = "http://qwen.hive-models.svc.cluster.local/v1/chat/completions"
credential = { kind = "none" }
```

Keyless runtimes and the built-in file tools are allowed only for endpoints
Hive considers local (`hive_core::endpoint::endpoint_is_local_server`). A
fully qualified `*.svc.cluster.local` name passes today because `.local` is in
the private-suffix list; a short name such as `qwen.hive-models` or
`qwen.hive-models.svc` does not, and neither does a cluster with a custom
cluster domain. Use the full name until the suffix list is configurable
(Section 12).

Serving options, from simplest to most capable:

- **vLLM** Deployment (or the vLLM production-stack Helm chart) with a GPU
  request, a model cache PVC and a ClusterIP Service.
- **KServe** `LLMInferenceService` (or `InferenceService` with the vLLM
  runtime): model lifecycle, scale-to-zero, OpenAI-compatible routes.
- **llm-d** with the **Gateway API Inference Extension**: an `InferencePool`
  behind a Gateway, with an endpoint picker that routes on KV-cache and queue
  state. Hive talks to the Gateway's OpenAI-compatible URL and needs no
  change.

All three are runtimes from Hive's point of view. GPU scheduling and
autoscaling belong to the model namespace, not to Hive pods (Section 8). A
hosted provider key (`LIGUS_WS_SECRET_<ref>`) remains the other choice, with
egress to that provider only.

## 4. Sandboxed execution

### 4.1 What needs a sandbox

1. **Agent turns with tools.** A turn can run `run_command`, a CLI agent in
   an edit or bypass mode, or stdio MCP servers, and its input includes text
   from anyone who can post in the chat (and, through MCP, from the internet).
   Treat it as untrusted code that holds whatever the turn was given.
2. **Untrusted pull-request tests.** Arbitrary code by definition
   (`pr-test-sandbox.md`): build scripts, test fixtures, install hooks.

In both cases the sandbox must hold no workspace key, device key, relay token,
forge credential or Kubernetes credential, must not reach the cluster network
beyond what the run was granted, and must survive hostile code (fork bombs,
disk fill, mining, escape attempts).

### 4.2 Backends

| | Plain Job + RuntimeClass | kubernetes-sigs/agent-sandbox | Agent Substrate (Google, open source) |
|---|---|---|---|
| What it is | `batch/v1` Job per turn, hardened pod, optional `runtimeClassName` (gVisor, Kata) | SIG Apps controller: `Sandbox` (`agents.x-k8s.io/v1beta1`), plus `SandboxTemplate`, `SandboxClaim`, `SandboxWarmPool` (`extensions.agents.x-k8s.io/v1beta1`) [1] | Actors multiplexed onto pre-started workers in a `WorkerPool`; idle actors snapshotted (memory and files) to object storage and restored in under a second; builds on Agent Sandbox [4][5] |
| Maturity | GA everywhere | v1.0.x (v1.0.11, 12 Nov 2026; `v1alpha1` removed in 1.0) [2]; managed as GKE Agent Sandbox, GA May 2026 [4] | announced May 2026; README states 1.0 API/CLI stability [5]; GKE Standard 1.37+ and Workload Identity required *(per Google docs)* [6] |
| Isolation | whatever RuntimeClass the cluster has; runc by default | none of its own, uses `runtimeClassName` (gVisor and Kata documented) [1] | gVisor or a Cloud Hypervisor microVM; no runc [5] |
| Start latency | cold pod start (seconds; image pull dominates) | warm pools: a claim binds a pre-started sandbox | sub-second restore of a snapshotted actor |
| Turn semantics | one-shot, natural fit: run, return, exit | stateful singleton pod with stable hostname, optional PVC, shutdown time; fits long-lived or multi-turn sessions | long-lived actors, suspend and resume |
| Queueing | Kueue native (`batch/v1` integration) | Kueue via the plain-Pod integration (queue label in `spec.podTemplate`), documented on both sides [3]; claims and warm pools not covered *(unverified)* | its own scheduler on pre-started workers |
| Portability | any conformant cluster | any cluster where the controller is installed (manifest or Helm) | GKE only in practice today; snapshots to a GCS bucket |
| Extra trust surface | none | a controller with cluster-wide permissions; the optional router and in-sandbox daemon | controller, node agent DaemonSet, Envoy proxy, gRPC API server; **snapshots put turn state (plaintext prompts and files) in a bucket** |

### 4.3 Recommendation

Make the sandbox a **pluggable backend** behind one interface in the
controller (`TurnExecutor`, Section 12), rendering the same hardened pod
(Section 4.4) whatever the backend:

1. **Default: plain Jobs.** No CRDs to install, works on every cluster
   including kind and k3d, one-shot semantics match a turn exactly, and
   Kueue admits Jobs natively. Isolation strength comes from the
   RuntimeClass, not from the API object: set `runtimeClassName: gvisor`
   (GKE Sandbox, or self-installed runsc) or a Kata class (AKS Pod
   Sandboxing names it `kata-vm-isolation` *(newer docs also show
   `kata-mshv-vm-isolation`; check `kubectl get runtimeclass`)*) on a
   dedicated node pool. The chart treats "no sandboxed RuntimeClass" as a
   warning for agent turns and as an error for PR tests unless explicitly
   overridden.
2. **Recommended where installed: agent-sandbox** (`SandboxClaim` against a
   `SandboxTemplate`, with a `SandboxWarmPool` for latency). It is the
   vendor-neutral Kubernetes API for exactly this problem, it is now `v1beta1`
   with a 1.x release line, and GKE offers it as a managed feature. Use it
   when cold-start latency matters (interactive chats) or when an agent needs
   a session that outlives one turn (a long CLI-agent session, a dev server
   the agent pokes at). The backend creates one claim per turn and deletes it
   after; warm pools hold images pulled and pods started, never turn data.
3. **Optional: Agent Substrate**, for GKE users who want many idle agent
   sessions at low cost. Off by default, because suspend-to-bucket writes
   turn plaintext to object storage; enabling it requires a bucket with
   customer-managed encryption keys in the workspace's own project, and the
   design must treat that bucket like the identity PVC. Open MCP connections
   do not survive a suspend, so Hive's MCP sessions must be re-established
   per resume.

Why not agent-sandbox as the default: it adds a cluster-scoped controller
that a small install does not otherwise need, and a turn is one-shot, so the
features it adds (stable identity, persistent storage, pause and resume) are
not needed on the common path. The interface is designed so switching backend
is a values change, and the acceptance tests in Phase 3 run against both.

Sources:
[1] https://github.com/kubernetes-sigs/agent-sandbox ;
[2] https://github.com/kubernetes-sigs/agent-sandbox/releases ;
[3] https://kueue.sigs.k8s.io/docs/tasks/run/external_workloads/sandbox/ ;
[4] https://cloud.google.com/blog/products/containers-kubernetes/bringing-you-agent-sandbox-on-gke-and-agent-substrate ;
[5] https://github.com/agent-substrate/substrate ;
[6] https://docs.cloud.google.com/kubernetes-engine/ai-ml/about-agent-substrate ;
GKE Sandbox (gVisor): https://docs.cloud.google.com/kubernetes-engine/docs/concepts/sandbox-pods ;
AKS Pod Sandboxing (Kata): https://learn.microsoft.com/en-us/azure/aks/use-pod-sandboxing ;
Kubernetes blog on agent-sandbox: https://kubernetes.io/blog/2026/03/20/running-agents-on-kubernetes-with-agent-sandbox

### 4.4 The hardened pod, whatever the backend

Every backend renders the same pod spec for an executor:

```yaml
spec:
  automountServiceAccountToken: false
  enableServiceLinks: false
  restartPolicy: Never
  runtimeClassName: gvisor            # optional, from values; required for PR tests
  securityContext:
    runAsNonRoot: true
    runAsUser: 65532
    runAsGroup: 65532
    fsGroup: 65532
    seccompProfile: { type: RuntimeDefault }
  containers:
    - name: turn
      image: <registry>/hive-executor@sha256:...   # pinned by digest
      args: ["exec-turn", "--bundle", "/in/turn.json"]
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        capabilities: { drop: ["ALL"] }
      resources:
        requests: { cpu: "500m", memory: 1Gi, ephemeral-storage: 2Gi }
        limits:   { cpu: "2",    memory: 4Gi, ephemeral-storage: 8Gi }
      volumeMounts:
        - { name: in,   mountPath: /in,   readOnly: true }
        - { name: work, mountPath: /work }
        - { name: tmp,  mountPath: /tmp }
  volumes:
    - { name: in,   secret: { secretName: turn-<id>, defaultMode: 0400 } }  # or a projected volume
    - { name: work, emptyDir: { sizeLimit: 8Gi } }
    - { name: tmp,  emptyDir: { sizeLimit: 1Gi } }
```

plus `activeDeadlineSeconds` on the Job (or the sandbox's shutdown time) for
the turn deadline, `ttlSecondsAfterFinished` for cleanup, `backoffLimit: 0`
(a failed turn is reported, not retried behind the user's back), and a
NetworkPolicy selected by the executor's labels: deny all ingress; egress only
to DNS, the controller's callback port, the model endpoint and the MCP
Services this turn may use. PR test runs get **no egress at all** (not even
DNS). A per-pod PID limit is a kubelet setting (`podPidsLimit`), so sandbox
nodes should set it; it cannot be set per pod.

The turn bundle is a Secret only if it is small (Secrets are capped near 1 MiB
and stored in etcd, where the turn's plaintext would then sit until deleted).
Larger bundles, and the git bundle, should be pulled by the executor from the
controller over the authenticated callback, which also avoids writing
plaintext to etcd at all. That is the default; the Secret path is a fallback
for tiny turns.

### 4.5 The workspace sandbox envelope on Kubernetes

Ligus 2.0 makes the envelope a workspace property
(`hive_core::sandbox::SandboxEnvelope`, enforced per runtime by
`hive_runtime::sandbox`). The executor should render it, not invent its own:

| Envelope field | Desktop / worker today | Executor pod |
|---|---|---|
| `isolate_writes` | per-turn git worktree; refuse if none | the `/work` emptyDir cloned from the git bundle; the controller turns the patch into a proposal |
| `protected_dirs` | built-in tools refuse writes; proposals never apply into them | same, plus `readOnlyRootFilesystem` |
| `egress: open` | runtime's own permission tier | NetworkPolicy: DNS, controller callback, model endpoint, granted MCP Services |
| `egress: allowList` | MCP filtered; Claude Code `--strict-mcp-config`, no web or shell tools; unconfinable runtimes refused | NetworkPolicy egress to the model endpoint and the allow-listed MCP Services only; no egress proxy route |
| `egress: closed` | no MCP, no web or shell tools | model endpoint only |
| `redact_secrets` | replies masked before they enter the log | same code path (the controller writes the log) |
| `human_gate` | every proposal needs a human approval to apply | same, enforced by the controller |

The bundle (§12, item 13) should carry the envelope verbatim so the
executor and the controller agree on it.

## 5. Ecosystem notes (as of November 2026)

Versions move fast; pin what the chart is tested against.

| Project | State (Nov 2026) | Relevance | Source |
|---|---|---|---|
| agent-sandbox | v1.0.11; `Sandbox` in `agents.x-k8s.io/v1beta1`, templates/claims/warm pools in `extensions.agents.x-k8s.io/v1beta1`; Python, Go and TypeScript SDKs. Some third-party docs (including Kueue's guide) still show the removed `v1alpha1` | sandbox backend | github.com/kubernetes-sigs/agent-sandbox |
| GKE Agent Sandbox / Agent Substrate | managed agent-sandbox GA May 2026; Substrate open source (Apache-2.0) | optional backend | cloud.google.com blog (above) |
| gVisor / Kata | GKE Sandbox `runtimeClassName: gvisor` (no hostPath, no port-forward); Kata Containers 4.0 (July 2026) with the Rust runtime default; AKS Pod Sandboxing on Azure Linux, no Arm64 | sandbox isolation | GKE and AKS docs; katacontainers.io blog |
| Kueue | v0.20.x, `kueue.x-k8s.io/v1beta1`; Jobs, plain Pods, pod groups, JobSet; agent-sandbox via plain-Pod integration | turn queueing and quota | github.com/kubernetes-sigs/kueue |
| KEDA | v2.21 (CNCF graduated); `prometheus`, `metrics-api`, `external` scalers fit a queue-depth signal | inference autoscaling | github.com/kedacore/keda |
| ToolHive operator | v0.65 (pre-1.0), `toolhive.stacklok.dev/v1beta1` `MCPServer` (stdio, SSE, streamable HTTP) behind a proxy Deployment; stdio limited to one backend replica and one client; **no auth unless an OIDC/authz reference is set** | MCP hosting | docs.stacklok.com/toolhive |
| kagent / kmcp | kagent 1.0.x (CNCF sandbox), `kagent.dev/v1alpha2` `RemoteMCPServer`; kmcp `MCPServer` (`kagent.dev/v1alpha1`) with stdio behind a gateway sidecar | MCP hosting | kagent.dev docs |
| Gateway API Inference Extension | v1.6.x; `InferencePool` GA at `inference.networking.k8s.io/v1`; `InferenceModel` replaced by `InferenceObjective`, which moved to llm-d in 1.6 | model routing | gateway-api-inference-extension.sigs.k8s.io |
| llm-d | v1.0.0 (Oct 2026, CNCF sandbox); OpenAI-compatible API via vLLM and its router | model serving | github.com/llm-d/llm-d |
| KServe | v0.21 (CNCF incubating); `LLMInferenceService` at `serving.kserve.io/v1alpha2`, built on llm-d; OpenAI-compatible `/v1/chat/completions` | model serving | github.com/kserve/kserve |
| vLLM production-stack | Helm chart `vllm/vllm-stack`, OpenAI-compatible router | model serving | docs.vllm.ai |
| cosign / syft / distroless | cosign v3 (Sigstore bundle format default); syft 1.5x; distroless now Debian 13 (`cc-debian13`, `static-debian13`) | supply chain | sigstore blog; GitHub releases |

For MCP, note that ToolHive's proxy accepts every request unless auth is
configured, so a Hive install using it must either configure OIDC or rely on
NetworkPolicy to make the Service reachable only from the workspace's own
pods.

## 6. Security model on Kubernetes

**End to end stays end to end.** The relay namespace holds the database
password, the relay's own token-issuer public key and (on the tunnel pod) the
APNs key. It never holds a workspace key, a device key or plaintext. The
workspace key, device keys, forge credentials and model keys live only in the
workspace namespace, in the controller's Secret and PVC. Executors get a
turn-scoped model credential and nothing else. Telemetry (Section 11) never
carries message content, prompts, file contents or tool arguments.

**Tenancy: one namespace per workspace.** `hive-ws-<name>` holds the serve or
worker StatefulSet, its Secrets and PVCs, its MCP servers, its executors and
its Kueue `LocalQueue`. This is the unit for RBAC, quotas, NetworkPolicies and
deletion. Shared namespaces: `hive-relay`, `hive-models` (optional),
`hive-tools` (optional, MCP servers holding no per-workspace secrets). Hard
multi-tenancy for mutually distrusting workspaces (a hosting provider) needs
more than namespaces: separate node pools for executors, sandboxed runtimes
(Section 4) and, ideally, a cluster per tenant or a virtual-cluster layer.

**Pod Security.** Label every Hive namespace
`pod-security.kubernetes.io/enforce: restricted`. All Hive pods run non-root,
drop all capabilities, use `RuntimeDefault` seccomp and a read-only root
filesystem with writable `emptyDir` or PVC mounts. Nothing needs
`hostNetwork`, `hostPath` or privileged mode; the chart refuses values that
would set them.

**RBAC.** The relay, MCP servers and executors run with service accounts that
have no Kubernetes API permissions and `automountServiceAccountToken: false`.
The controller (Phase 3) gets a Role in its own namespace allowing
`create`/`get`/`watch`/`delete` on `jobs` (or `sandboxclaims`), `create`/
`delete` on `secrets` only if the Secret bundle fallback is enabled, and
`get` on `pods/log` for diagnostics. No ClusterRoles, no access to other
namespaces.

**NetworkPolicies** (default deny in every Hive namespace, then allow):

| From | To | Why |
|---|---|---|
| ingress controller | `relay-*:8443` | clients |
| `relay-*` | Postgres | store |
| `hive-serve` | relay (egress, cluster or public), model endpoints, MCP Services, forge (GitHub) | sync, turns, git |
| tailnet proxy or nothing | `hive-serve:8787` | phone API only if not via tunnel |
| executors | controller callback, model endpoint, granted MCP Services, DNS | turn |
| PR-test executors | nothing | untrusted code |
| `hive-models` | nothing outbound except model registries during download | inference |

NetworkPolicy cannot match DNS names, so egress to hosted model providers or
GitHub is either a broad CIDR allow, a CNI with FQDN policies (Cilium, Calico
Enterprise, Antrea), or an egress proxy with a hostname allowlist that
executors must use. The last one is portable and is the recommendation for
executors that need any internet egress.

**Secrets.** Kubernetes Secrets as the interface, filled by External Secrets
Operator (from a cloud secret manager or Vault) or by SOPS-encrypted files in
Git (Flux or Argo CD with a SOPS plugin). Enable etcd encryption at rest (KMS
provider on managed clusters). Rotation: the workspace key rotates by Hive's
own mechanism (rotate in the app, update the Secret, restart); relay tokens
and model keys rotate by updating the Secret and restarting the pod (Hive
reads them at start).

**Images.** Pull by digest, verify cosign signatures at admission (Sigstore
policy-controller or Kyverno `verifyImages`), and keep executor images
minimal: the executor image is attack surface the agent controls.

## 7. Egress control for agents

Agents are the component most likely to be talked into exfiltrating data.
Layers, from Hive outward:

1. Hive's own policy: tools only for requesters at `LIGUS_MCP_MIN_ROLE` or
   above, MCP refused after file reads, built-in file tools only on local
   model endpoints, `run_command` off by default.
2. The executor holds nothing worth stealing beyond the turn's own inputs.
3. NetworkPolicy: executors reach the controller, the model endpoint and
   granted MCP Services only.
4. For internet egress (hosted model API, package indexes during installs),
   an egress proxy with an explicit hostname allowlist per workspace.
5. PR tests: no network.

## 8. Scheduling, autoscaling and cost

- **Kueue** admits executor Jobs (and agent-sandbox pods through the
  plain-Pod integration, see Section 5)
  against quotas: a `ClusterQueue` per tier or tenant with CPU, memory and
  GPU quotas, a `LocalQueue` in each workspace namespace, cohorts for
  borrowing idle quota, and fair sharing so one busy workspace cannot starve
  the rest. The controller labels each executor with
  `kueue.x-k8s.io/queue-name`. Queued turns show as "queued" on the phone and
  desktop through the existing turn states.
- **Node pools.** A dedicated, tainted pool for executors (and, if used, the
  gVisor or Kata runtime), so untrusted code never shares a node with the
  controller or the relay. GPU pools for inference, with the vendor's device
  plugin or DRA driver, taints, and node affinity on the model servers only.
  Executors rarely need a GPU: the model runs elsewhere.
- **Autoscaling.** Relay: HPA on CPU (after the pairing-code change). Serve
  and worker StatefulSets: never autoscaled. Executors: not scaled, created
  per turn; the cluster autoscaler or Karpenter adds executor nodes when Kueue
  admits more work than fits. Inference: KEDA or the serving stack's own
  autoscaler on queue depth (vLLM's waiting-requests metric, or the
  controller's `hive_turns_queued` gauge), including scale to zero for
  rarely used models.
- **Cost.** The fixed cost per workspace is one small pod (serve) plus a PVC.
  The relay is shared and small. Spend scales with executors (CPU-seconds per
  turn) and inference (GPU-hours); Kueue quotas are the cost control, and
  spot or preemptible nodes suit executors because a lost turn is reported
  and can be asked again.

## 9. Images

| Image | Base | Contents |
|---|---|---|
| `hive` | `debian:bookworm-slim` (this change: `deploy/container/Dockerfile`, about 290 MB uncompressed, most of it git and its dependencies) | `hive` binary, git, CA certificates, tini; UID 65532 |
| `hive-executor` (Phase 3) | same, or a Wolfi/Chainguard base | `hive` binary for `exec-turn`, git, plus whichever CLI agents and toolchains a workspace allows; one variant per toolchain |
| `hive-relay` | today Alpine; proposed distroless static | relay binary (relay repo) |

Notes:

- The `hive` binary shells out to `git` for worktrees, so a fully distroless
  image needs a git binary added; a slim Debian or Wolfi base is the
  pragmatic choice. The release workflow already builds static musl binaries
  for `x86_64` and `aarch64`, so the image could also be assembled from the
  release assets instead of compiling in Docker.
- Multi-arch (`linux/amd64`, `linux/arm64`) with `docker buildx`, pushed to
  GHCR, signed keyless with cosign from GitHub Actions OIDC, with an SBOM
  (syft, or buildx `--sbom=true`) and SLSA provenance (`--provenance=mode=max`)
  attached as attestations.
- CI: build the image on every PR that touches `crates/` or
  `deploy/container/`, run `hive --version` and a kind smoke test (Section 13),
  scan with Trivy or Grype, and publish only from release tags.

## 10. Packaging

**Helm first, Kustomize-friendly.** An umbrella chart `hive` with subcharts:

| Subchart | Renders | Notes |
|---|---|---|
| `relay` | `relay-sync` and `relay-tunnel` Deployments, Services, HTTPRoute/Ingress, PDB, ServiceMonitor | Owned by the relay repo, published as an OCI chart, pulled as a dependency; optional (many installs use an existing relay) |
| `serve` | StatefulSet, Service, NetworkPolicies, ConfigMaps (runtimes, MCP, dashboard), Role (Phase 3) | `workspaces:` list renders one StatefulSet per workspace, each in its own namespace |
| `worker` | sharded StatefulSet (Phase 1) | optional |
| `mcp` | Deployment and Service per configured server, or ToolHive/kagent CRs when `mcp.provider` is set | optional |
| `executor` | RuntimeClass references, Kueue `LocalQueue`s, executor NetworkPolicy, pod template ConfigMap | Phase 3 |

Helm because installs need conditional logic (relay external or in-chart,
tunnel on or off, sandbox backend, MCP provider) and a values schema
(`values.schema.json`) that refuses unsafe combinations (`hostNetwork`, public
serve Ingress without the explicit flag, an executor image without a digest).
Kustomize users render with `helm template` or use Kustomize's Helm chart
inflation; plain manifests generated from the chart go in
`deploy/kubernetes/manifests/` for kind. Charts are published to GHCR as OCI
artifacts and signed with cosign like the images.

**An operator, later and only if needed.** CRDs such as `HiveWorkspace`
(serve StatefulSet, Secrets wiring, enrollment status, tunnel status),
`HiveAgentPool` (executor template, backend, queue, egress grants) and
`HiveWorker` earn their keep when one of these is true:

- many workspaces per cluster (a hosting provider) where per-workspace
  namespaces, quotas, Secrets and NetworkPolicies must be stamped out and
  garbage-collected;
- lifecycle steps that are imperative today need to be automated (enrollment
  approval status, tunnel enable, key rotation, backup and restore of the
  identity PVC);
- the executor dispatcher outgrows "the controller creates Jobs" (warm
  pools, preemption handling, cross-workspace fair sharing beyond Kueue).

Until then the controller's own Job creation plus Helm is enough, and an
operator would mostly duplicate Helm.

## 11. Observability

- **Health.** Relay: `GET /v1/health` (store round trip) for readiness and
  liveness. Serve: `GET /v1/health` (unauthenticated). Worker: none today;
  add an optional health listener (Section 12). Readiness for serve and
  worker should mean "first sync completed and the turn loop is running";
  liveness should catch a wedged sync or turn loop (a heartbeat timestamp the
  loop updates, checked by the handler).
- **Metrics.** Relay: `/metrics` already (request totals, in-flight, status
  classes; content-blind). Serve and worker: add a Prometheus `/metrics`
  endpoint with sync rounds and failures, sync lag (relay head minus local
  cursor), turns by state and runtime, turn duration, queued turns, tool
  calls by tool name and outcome, executor dispatch latency, tunnel
  connection state and paired-phone count. No labels with chat titles, user
  names or message content. A `ServiceMonitor` or `PodMonitor` in the chart.
- **Traces and metrics over OTLP.** `hive serve`, `hive worker` and
  `hive exec-turn` emit OpenTelemetry spans and metrics when the standard
  `OTEL_EXPORTER_OTLP_ENDPOINT` (and `OTEL_SERVICE_NAME`,
  `OTEL_RESOURCE_ATTRIBUTES`) are set, and nothing otherwise. Spans: sync
  round, turn (claim, dispatch, model call, tool call, proposal filing),
  executor lifecycle; the controller propagates trace context to the executor
  in the turn bundle. Attributes carry ids, runtime and model names, token
  counts and durations, never prompts, replies, file paths from the user's
  repo or tool arguments. The relay can emit HTTP server spans the same way.
- **Logs.** JSON logs on stdout behind `LIGUS_LOG_FORMAT=json`, with the same
  content rule. `hive pair` output never goes to a log.

## 12. Code changes, prioritized

**P0: needed for a correct Phase 1 install.**

1. **Explicit in-pod bind.** Accept `LIGUS_SERVE_LISTEN=0.0.0.0:8787` when
   running in a pod (`KUBERNETES_SERVICE_HOST` set) with a startup log line,
   or document `LIGUS_SERVE_ALLOW_ANY_BIND=1`; and make `hive pair` refuse a
   pairing URL built from an unspecified address (today it only catches
   loopback), pointing at `--public-url` or `--relay-only`.
2. **`LIGUS_RUNTIMES_FILE`** (and the same for `repo-bindings.toml` if needed)
   so ConfigMaps can be mounted read-only instead of copied into the PVC.
3. **Configurable stop grace.** `LIGUS_STOP_GRACE_SECS` (default 15) so
   `terminationGracePeriodSeconds` and the turn grace agree; log what was
   interrupted. `hive worker` needs the same treatment: it checks for a
   signal only between ticks, and it installs its SIGTERM handler on the first
   wait, so a SIGTERM during the very first turn kills it outright and one
   during a later turn waits for the turn however long it takes (then the
   kubelet's SIGKILL lands mid-turn). Install the handler at start and bound
   the wait like serve does.
4. **Worker health endpoint.** `hive worker --health-listen 0.0.0.0:9090`
   serving `/healthz`, `/readyz` (first sync done) and, with P1-6, `/metrics`.
5. **Cluster-internal model endpoints.** Treat `*.svc`, `*.svc.<cluster
   domain>` and names from `LIGUS_PRIVATE_HOST_SUFFIXES` as local in
   `endpoint_is_local_server`, so short Service names and custom cluster
   domains work for keyless runtimes and file tools.

**P1: production quality.**

6. Prometheus `/metrics` on serve and worker; OpenTelemetry OTLP export
   (Section 11); `LIGUS_LOG_FORMAT=json`.
7. Readiness that reflects sync state; liveness from a loop heartbeat.
8. **Relay (other repo):** pairing codes in the store so `relay-sync` can
   scale; Postgres `LISTEN/NOTIFY` for SSE fan-out; a non-root image without
   the `chown` entrypoint; confirm and document client-IP handling behind a
   proxy.
9. Secrets from files as well as env (`LIGUS_WORKSPACE_KEY_FILE`,
   `LIGUS_RELAY_ACCESS_TOKEN_FILE`, `LIGUS_WS_SECRET_DIR`), so Secrets can be
   mounted as files and rotated without exposing them in the process
   environment.
10. GitHub App authentication for clone, fetch, push and pull requests, as an
    alternative to the `gh` login (relevant once the phone repo features from
    the `feat/serve-repos` work are released).

**P2: the controller/executor split (Phase 3).**

11. A `TurnExecutor` trait in the worker with the current in-process
    implementation and a Kubernetes implementation (Jobs; agent-sandbox
    behind a feature flag).
12. `hive exec-turn`: runs one turn from a bundle with no identity, no
    `hive.db` and no relay access; streams deltas and returns reply plus
    patch.
13. Turn bundle format (versioned JSON: prompt, context, runtime, tool
    grants, deadlines, trace context, a one-time callback token) and git
    bundle handoff.
14. Concurrency in the controller: N turns in flight per workspace instead of
    one, bounded by a setting and by Kueue.
15. PR tests ("Run PR tests") on the same executor path with a no-network
    profile.

**P3: only if demand appears.**

16. Several workspaces per `hive serve` process (today one per process; one
    StatefulSet per workspace is fine until there are hundreds).
17. Agent pools spanning several hosts (an agent bound to a pool, with
    lease-based claims) if a single controller per workspace ever becomes
    the bottleneck.
18. A desktop-independent workflow run engine on serve (workflow runs are
    started and driven from the desktop today; stages already run on worker
    hosts).
19. The operator (Section 10).

Not needed: leader election for `hive serve` or `hive worker`. Each is a
single identity in a single-replica StatefulSet, and the StatefulSet already
guarantees at most one pod per ordinal (except during a node partition with
forced deletion, which operators must not do for these pods). The relay
tunnel is likewise pinned to one replica by its Deployment.

## 13. From a laptop to a managed cluster

**kind or k3d (Phase 1 quickstart).**

1. `kind create cluster` (or `k3d cluster create`).
2. Build and load the image: `docker build -f deploy/container/Dockerfile -t
   hive:dev .` then `kind load docker-image hive:dev`.
3. Install the relay with the memory store and a PVC (Postgres optional),
   `LIGUS_RELAY_TUNNEL=open`.
4. Create the workspace Secret from a desktop invite (relay URL, room, key),
   install the serve StatefulSet, and point it at the in-cluster relay.
   For the desktop to reach the same relay, `kubectl port-forward
   svc/relay 8443` and use `http://127.0.0.1:8443` with the desktop's
   insecure-relay option *(check the desktop's flag name)*, or install a TLS
   Ingress with a local CA.
5. A stub OpenAI-compatible server (a few lines of Python or a small vLLM
   CPU model) as the runtime, so turns run without a GPU or API key.

**Managed clusters (GKE, EKS, AKS).** Same chart, plus: managed Postgres for
the relay; an encrypted StorageClass; External Secrets; Gateway API with
cert-manager; a tainted executor node pool with gVisor (GKE Sandbox) or Kata
(AKS Pod Sandboxing) where available; Kueue; and the cloud's GPU node pools
for inference.

## 14. Phased plan and acceptance tests

**Phase 0: design and image (this change).**

- `docker build -f deploy/container/Dockerfile .` succeeds on amd64 and arm64;
  `docker run <image> --version` prints the workspace version; the container
  runs as UID 65532.

**Phase 1: run today's binaries on Kubernetes.**

Deliverables: image CI (multi-arch, signed, SBOM); relay chart in the relay
repo; `serve` and `worker` subcharts; NetworkPolicies; P0 code changes;
kind quickstart.

Acceptance (automated in CI on kind):

1. Relay `GET /v1/health` is 200 through the Service; `/metrics` serves.
2. `hive-serve-0` starts, completes its first sync and reports ready; it
   appears in `hive hosts` from a second client (a CLI pod with its own data
   dir standing in for the desktop).
3. The second client posts `@agent hello` to an agent bound to the serve
   host; a reply from the stub model arrives within 30 s.
4. Deleting `hive-serve-0` mid-turn: the pod comes back on the same PVC with
   the same host id, and the interrupted reply is finished as interrupted.
5. The relay's database contains no plaintext: grep every envelope body for a
   marker string posted in step 3 and find nothing.
6. All Hive namespaces pass `restricted` Pod Security admission; a pod in an
   unrelated namespace cannot reach `hive-serve:8787` (NetworkPolicy).
7. A `relay-tunnel` rollout (`Recreate`) reconnects the serve host within
   30 s and a tunnel client completes a request afterwards.

**Phase 2: observability and hardening.**

Deliverables: P1 code changes; ServiceMonitors; dashboards; External Secrets
example; image signature verification policy.

Acceptance:

1. Prometheus scrapes serve, worker and relay; `hive_turns_total` increments
   per turn; no metric label contains message text (test with a marker).
2. With an OTLP collector configured, a turn produces a trace spanning
   claim, model call and proposal; exported spans contain no marker text.
3. Readiness flips to not-ready when the relay is unreachable at start, and
   liveness restarts a pod whose loop heartbeat stops.
4. An unsigned image is refused by admission.

**Phase 3: controller and executors.**

Deliverables: P2 code changes; `executor` subchart with the Job backend;
Kueue integration; agent-sandbox backend behind a flag; PR-test profile.

Acceptance:

1. Ten mentions to one agent in one workspace run as ten executor Jobs,
   admitted up to the `LocalQueue` quota, and all ten replies and proposals
   arrive; no turn is answered twice.
2. Inside an executor: no `LIGUS_WORKSPACE_KEY`, no device key files, no
   service account token (`/var/run/secrets/kubernetes.io` absent), no git
   credentials; `getent hosts github.com` fails unless the turn was granted
   forge egress; the root filesystem is read-only.
3. An executor that exceeds its memory or deadline is reported as a failed
   turn on the chat, and its Job is cleaned up.
4. A turn that writes files produces a `FileDiff` proposal identical to the
   in-process path for the same input (golden test with a scripted model).
5. PR tests: a test that tries the network, reads environment secrets and
   forks repeatedly fails safely; the log is posted as untrusted text; the
   run leaves no pod, Secret or volume behind.
6. With `runtimeClassName: gvisor` on a cluster that has it, the same suite
   passes and `dmesg` inside the pod shows the gVisor kernel.

**Phase 4: scale-out and optional operator.** Relay `relay-sync` with more
than one replica (after pairing codes move to the store); several workspaces
per serve process if needed; operator if the conditions in Section 10 hold.

## 15. Open questions

1. **Executor protocol.** Callback over HTTP to the controller's Service, or
   a result volume plus the Kubernetes API for streaming logs? The callback
   streams deltas cleanly but adds an authenticated endpoint to the
   controller.
2. **CLI agents in executors.** Claude Code, Codex, Antigravity and Copilot
   CLIs keep their login in their own config on the host. In an executor that
   means mounting a subscription login into untrusted code. Allow it only for
   API-key runtimes in executors, and keep subscription CLIs on a trusted
   worker StatefulSet?
3. **Bundle size and repos.** Where is the cutoff between a git bundle and an
   ephemeral clone, and should the controller cache bundles per base commit?
4. **Owner-device enrollment for many workspaces.** Approving each serve pod
   from the desktop is fine for a few workspaces; a provider hosting hundreds
   would want a bulk flow. Is that a product goal?
5. **One controller per workspace or a shared multi-workspace serve?** The
   per-workspace StatefulSet is simple and isolates keys per namespace; a
   shared process would save memory but mix keys in one pod.
6. **Relay tunnel at scale.** Beyond one tunnel replica, route
   `/v1/tunnel/connect?host=H` and `/v1/tunnel/accept` to the replica holding
   H's control connection (consistent hashing on `host` at the proxy, or a
   relay-side redirect). Needed only when one replica's socket count is the
   limit.
7. **Workflows without a desktop.** Should the serve pod gain a run engine so
   scheduled or event-triggered workflows run fully in-cluster?
8. **Backups.** Is backing up the identity PVC desirable (fast restore of a
   host id) or should a lost PVC mean "enroll a new device" (simpler, no key
   material in backups)?

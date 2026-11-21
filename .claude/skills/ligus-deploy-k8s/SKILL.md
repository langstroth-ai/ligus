---
name: ligus-deploy-k8s
description: Deploy, verify, upgrade or tear down a Ligus workspace (ligus serve, workers) on Kubernetes with the deploy/helm/ligus chart, using deploy/kubernetes/ligus-k8s. Use when asked to run Ligus on a cluster (OrbStack, kind, k3d, EKS, GKE, AKS, ...), to try the Kubernetes shape locally, or to check a cluster can host Ligus.
---

# Deploy Ligus on Kubernetes

Read the repository's `AGENTS.md` first: its hard rules (no secret values,
no weakened defaults) apply to every step. Tool: `deploy/kubernetes/ligus-k8s`.
`ligus-k8s --help` prints every command, flag, default and exit code;
`ligus-k8s help <command>` (or `<command> --help`) prints one command. The
ligus CLI's guides cover the background: `ligus help kubernetes`,
`ligus help agents-guide`, `ligus help troubleshooting`, `ligus help security`. Always pass `--context`, `-n` and `--release`, and add
`--json` to read results; exit 4 means **stop and ask the human**.

Set these once and reuse them:

```sh
K=deploy/kubernetes/ligus-k8s
CTX=<kube context>   NS=<namespace, e.g. ligus-ws-acme>   R=<release, e.g. acme>
IMG=<repository:tag of the Ligus image, 0.1+>
```

## 1. Decide the scenario (checkpoint: human)

| Scenario | When | Needs |
|---|---|---|
| `trial` | a local or throwaway cluster, to see it work | nothing from the human if the cluster is local |
| `team-in-cluster-relay` | a team's workspace; relay in this cluster (namespace, default `langstroth-relay`) | relay installed first (relay repository's AGENTS.md); the workspace Secret files |
| `team-external-relay` | a team's workspace; relay elsewhere at `https://` | `--relay-url https://...`; the workspace Secret files |

**Stop and confirm with the human**: the context `$CTX` (unless it is a local
cluster like `orbstack`, `kind-*`, `k3d-*`, `docker-desktop`, `minikube`),
the namespace, and the scenario. For team scenarios also ask which
StorageClass to use and whether any worker hosts are wanted (`--workers a,b`).

## 2. Image and relay chart

- The `ligus` image is not published. If `$IMG` doesn't exist, build it:
  `docker build -f deploy/container/Dockerfile -t ligus:dev .` (kind: then
  `kind load docker-image ligus:dev --name <cluster>`; OrbStack and Docker
  Desktop see local images directly). The relay image is published
  (`ghcr.io/langstroth-ai/langstroth-relay:0.3.0`, the trial's default);
  pass `--relay-image-repository R --relay-image-tag T` only for a mirror or
  your own build.
- The chart needs the relay chart in `deploy/helm/ligus/charts`. By default
  install pulls the published one (`oci://ghcr.io/langstroth-ai/charts`,
  anonymous). Offline, pass `--relay-chart <relay-checkout>/deploy/helm/langstroth-relay`
  (a full checkout: the chart links `../langstroth-postgres`) or
  `--fetch-relay-chart` (git clone of `langstroth-ai/langstroth-relay` at the
  commit CI pins) to the first install.

## 3. Preflight

```sh
$K preflight --context $CTX -n $NS --release $R --probe --image $IMG --json
```

Read every check. `--probe` creates and deletes one temporary namespace
(`ligus-preflight-*`) to measure Pod Security, NetworkPolicy enforcement and
the Landlock sandbox (it runs `ligus doctor --turn-test` in a restricted pod).

- `sandbox.*` fail: the nodes can't confine agents (need Linux 6.7+, Landlock
  in the LSM list; gVisor never works). **Stop**: the human picks other nodes.
  Never set `agentSandbox.require=false`.
- `cni.networkpolicy*` fail: the CNI doesn't enforce NetworkPolicy. **Stop**.
- `storage.rwop` warn on a team scenario: ask the human for a CSI StorageClass
  (`--storage-class`); `--access-mode ReadWriteOnce` is their call.
- `psa.namespace` warn: the namespace exists without `restricted`. **Stop**;
  the human labels it or picks a new namespace.

## 4. The workspace Secret

Trial:

```sh
$K secrets workspace --throwaway --context $CTX -n $NS --release $R --json
```

Real workspace (**checkpoint: human**). Ask the human to run, in their own
terminal, with values from the desktop app's invite (Settings > Team sync):

```sh
umask 077; mkdir -p ~/ligus-ws-secret
printf '%s' '<room>' > ~/ligus-ws-secret/room
printf '%s' '<workspace key>' > ~/ligus-ws-secret/key
printf '%s' '<hrt1 token>' > ~/ligus-ws-secret/relay-access-token   # only if the relay needs one
```

Never ask them to paste the values to you. Then:

```sh
$K secrets workspace --from-dir ~/ligus-ws-secret --context $CTX -n $NS --release $R --json
```

and tell them to delete the directory. Add `--relay-access-token` to the
install if they wrote that file. Other credentials (model keys for
`serve.agentEnv`, `serve.workspaceSecrets`) follow the same pattern with
`$K secrets generic --name NAME --from-dir DIR`, referenced from a values
file (`--values`) by Secret name only.

Real workspace: serve and every worker join as their own service member,
each with its own single-use **admission** (**checkpoint: human**). The
human makes one per box on a desktop (workspace menu > Admit a box > For
Kubernetes) and saves it as a file named `admission` in a private directory;
then:

```sh
$K secrets generic --name $R-serve-admission --from-dir ~/ligus-serve-admission --context $CTX -n $NS --release $R --json
```

and name it in a values file passed to install with `--values` (Secret
names only, never values):

```yaml
serve:
  admission:
    existingSecret: <R>-serve-admission   # workers: worker.instances[].admission.existingSecret
```

Tell the human to delete the directory, and the Secret once the box has joined. The box refuses to start
without a copy of the workspace and never founds one; if serve exits with a
membership or role refusal, **stop** and hand over (`ligus help box`,
`docs-site/docs/operate/deploy/kubernetes.md`). On a fresh throwaway
workspace, serve and workers started together race to found it (see
`deploy/helm/ligus/ci/kind-values.yaml`); if one restarts with a role
refusal, give the workers their own release (`--no-serve --workers a`).

## 5. Render, then install

```sh
$K install --scenario <S> --context $CTX -n $NS --release $R --image-repository <repo> --image-tag <tag> \
  [--relay-chart ... | --fetch-relay-chart] [--relay-namespace NS | --relay-url https://...] \
  [--workers a,b] [--storage-class SC] [--values extra.yaml] --dry-run --json
```

The dry run renders the chart, checks the rendered manifest keeps every
fail-closed default (exit 4 if not), and runs a server-side dry run
(admission, Pod Security). Show the human a summary for team scenarios, then
run the same command without `--dry-run` (it waits for readiness;
`--timeout`, default 10m).

## 6. Verify

```sh
$K verify --context $CTX -n $NS --release $R --json
```

All of `release.status`, `pod.*`, `sandbox.*` (Landlock ABI and a confined
agent turn), `serve.health`, `networkpolicy` and `helm.test` (a sandboxed turn
and sync through the relay on a throwaway workspace) should pass. A pod in
`Init`: `kubectl -n $NS logs <pod> -c sandbox-preflight`.

## 7. Hand over (checkpoint: human)

Report what was installed and the verify result, then list what only the
human does: pairing the phone (`kubectl -n $NS exec -it $R-ligus-serve-0 --
ligus pair --relay-only` in their own terminal, needs `remote.enabled`),
approving the box on a desktop, any exposure (tailnet proxy, never a public
Ingress for serve), DNS/TLS for the relay.

## Upgrade

Same `install` command with the new `--image-tag` (it runs `helm upgrade
--install`; there is no separate upgrade command); then `verify`. Relay
first, then workspaces. Changing access mode or the worker layout needs
`kubectl delete statefulset --cascade=orphan` first: ask the human.

## Tear down

```sh
$K teardown --context $CTX -n $NS --release $R --json                       # keeps PVCs and Secrets (alias: uninstall)
$K teardown --context $CTX -n $NS --release $R --delete-data --delete-secrets --delete-namespace --json
```

`--delete-secrets` deletes only this release's Secrets. `--delete-namespace`
only deletes namespaces ligus-k8s created and refuses while other Helm
releases remain in it. Deleting
data is irreversible: confirm with the human unless it is a throwaway
trial. Then remind them that uninstalling is not revoking.

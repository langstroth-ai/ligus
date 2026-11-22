# AGENTS.md

Instructions for coding agents (Claude Code, Codex, Copilot CLI, and others)
that **deploy or operate Ligus** from this repository. Ligus is short for
*ligustica*, the Italian honeybee. For changing the code itself, see
[CONTRIBUTING.md](CONTRIBUTING.md).

Step-by-step skills: [`.claude/skills/ligus-deploy-k8s`](.claude/skills/ligus-deploy-k8s/SKILL.md)
(Kubernetes) and [`.claude/skills/ligus-deploy-local`](.claude/skills/ligus-deploy-local/SKILL.md)
(this machine). Agents that don't load skills can read those files directly.

## Pick the deployment

| Goal | Deploy | Tool | Human-only parts |
|---|---|---|---|
| One person, one machine | The desktop app only | none: `brew install --cask langstroth-ai/ligus/ligus-app` | First launch, passkey and sign-in, creating the workspace |
| Agents keep working while the laptop sleeps (a home box, a spare Linux machine) | `ligus serve` (and optionally `ligus worker`) next to the desktop | `deploy/local/ligus-local` | approving the box admission (`ligus admit request`, then `ligus admit redeem`), pairing the phone, exposure (LAN, tailnet) |
| Try the Kubernetes shape on a local cluster (OrbStack, kind, k3d) | relay + serve in one namespace, throwaway workspace | `deploy/kubernetes/ligus-k8s`, scenario `trial` | none, unless the cluster is shared |
| A team's always-on parts on a cluster, relay in the same cluster | one `ligus` release per workspace | `ligus-k8s`, scenario `team-in-cluster-relay` (relay installed first from the relay repository) | the workspace connection (Secret files), the cluster and context, storage class, exposure |
| The same, relay outside the cluster (https) | one `ligus` release per workspace | `ligus-k8s`, scenario `team-external-relay` | as above, plus DNS and TLS for the relay |
| Self-host the relay | the relay | the relay repository (`github.com/langstroth-ai/langstroth-relay`) and its AGENTS.md | DNS, TLS, cloud credentials |

Kubernetes is a **preview**. The `ligus` chart and image are not published:
build the image from `deploy/container/Dockerfile` and install the chart from
this checkout. The relay chart and image (0.3.0) are published to
`ghcr.io/langstroth-ai`; install pulls the relay chart from there, or from a
relay checkout (`--relay-chart`) or a pinned fetch (`--fetch-relay-chart`).

## Commands

The `ligus` CLI documents itself: `ligus help` lists every command and
guide, `ligus <command> --help` gives a command's flags, defaults,
environment variables, examples and exit codes, and
`ligus help agents-guide` is a one-shot playbook for agents (set up a box,
add an agent, deploy on Kubernetes, check health). `ligus status --json`
summarises a data dir.

Every command is non-interactive and idempotent, takes flags or environment
variables (never prompts), prints progress on stderr and, with `--json`,
exactly one JSON object on stdout (schema `ligus.agent-deploy.v1`: `ok`,
`exitCode`, `checks[]` of `{id, status, detail, fix}`, `next[]`). No command
prints a secret.

Kubernetes (`--context` is required, never the current context; one namespace per workspace):

```sh
K=deploy/kubernetes/ligus-k8s
$K preflight --context CTX -n NS --release R --probe --image REPO:TAG --json   # read-only, plus a temporary probe namespace
$K secrets workspace --throwaway --context CTX -n NS --release R   # trial only
$K secrets workspace --from-dir DIR --context CTX -n NS --release R   # real workspace: DIR prepared by the human
$K install --scenario trial --context CTX -n NS --release R --image REPO:TAG \
   [--relay-image-repository RREPO --relay-image-tag RTAG] [--relay-chart RELAY/deploy/helm/langstroth-relay] --dry-run --json
$K install ...the same flags without --dry-run... --json   # waits for readiness (--timeout, default 10m)
$K verify --context CTX -n NS --release R --json                   # pods, sandbox preflight, health, NetworkPolicies, helm test
$K teardown --context CTX -n NS --release R [--delete-data --delete-secrets --delete-namespace] --json
```

This machine (always pass `--data-dir`; never the desktop app's. If you pass
`--env-file`, pass the same one to every command):

```sh
L=deploy/local/ligus-local
$L preflight --role serve --data-dir ~/.ligus-serve --json
$L install-cli [--version vX.Y.Z | --from-source] [--bin-dir DIR]   # until 0.1.0 is released: --from-source
$L configure --role serve --data-dir ~/.ligus-serve [--listen ADDR] [--label NAME] --json
#   then admit the box: ligus admit request; the HUMAN (an Admin) approves it; ligus admit redeem <code>
$L start --role serve --data-dir ~/.ligus-serve [--systemd | --launchd] --json
$L verify --role serve --data-dir ~/.ligus-serve --json   # alias: status
$L stop --role serve --data-dir ~/.ligus-serve [--systemd | --launchd]
$L uninstall --role serve --data-dir ~/.ligus-serve [--delete-data]
```

### Exit codes

| Code | Meaning | What the agent does |
|---|---|---|
| 0 | done (warnings allowed) | read the `warn` checks, continue |
| 1 | a check or an operation failed | read `checks[].fix`, fix, rerun |
| 2 | usage error | fix the command line |
| 3 | a precondition is missing that you can fix (a tool, the relay chart, the namespace) | do what `fix` says, rerun |
| 4 | **a human must decide or act** | stop and ask; quote `fix` and `next` |

## Hard rules

1. **Never handle secret values.** The workspace key, `hivews1:` invites,
   relay access tokens (`hrt1...`), registration tokens, pairing links, model
   API keys, database DSNs and passwords never go in chat, logs, command
   lines, values files, commit messages or files you write. Generate them
   into Secrets with `ligus-k8s secrets ...`; a real workspace's files are
   written by the human into a private directory (`umask 077`; files `room`,
   `key`, optionally `relay-access-token`) that you pass by path only. Never
   `cat`, `echo`, `kubectl get secret -o yaml`, or `base64 -d` them.
2. **Never weaken a fail-closed default to make an install pass.** That
   includes `agentSandbox.*` (the Landlock sandbox and its preflight),
   `LIGUS_REQUIRE_SANDBOX`, NetworkPolicies and `restrictEgress`, Pod Security
   `restricted`, TLS (`https://` relays, `insecureRelay`), Service type
   (`ClusterIP` only; no Ingress or LoadBalancer for serve), the listen
   address (`0.0.0.0` is refused) and auth on the relay. `ligus-k8s install`
   checks the rendered manifest and exits 4 if any is off. Stop and ask.
3. **Confirm the target.** Say which kube context and namespace (or which
   machine and data dir) you are about to change, and get a yes for anything
   that isn't a local throwaway cluster.
4. **One data dir is one device.** Never point two processes at one data dir
   or PVC, never use the desktop app's data dir, never put it on NFS/RWX.
5. **Uninstalling is not revoking.** After teardown of anything that joined a
   real workspace, tell the human to revoke the device or member from a
   desktop and to decide about rotating the workspace key.
6. Throwaway workspaces (`--throwaway`) are for trials and tests only; never
   use one for real data.

## Human-only steps

- **Passkeys and sign-in** on the desktop app (first launch, langstroth-auth
  device-code logins).
- **Invites, admissions and enrollment**: copying a workspace invite,
  `ligus enroll` and `ligus join` (a person's own CLI device), approving a
  box admission with a passkey (a box joins only with `ligus admit request`
  and `ligus admit redeem`, never an invite), approving a device on the
  desktop, and preparing the workspace Secret files for a cluster install.
- **Pairing the phone** (`ligus pair` prints a secret; run it in the human's
  own terminal, never as a Job or in a logged session).
- **Exposure**: anything reachable from outside the machine or cluster (LAN,
  tailnet, Ingress, LoadBalancer, NodePort), **DNS and TLS** certificates.
- **Cloud credentials**, the choice of cluster, StorageClass and node pools,
  and labelling an existing namespace Pod Security `restricted`.
- Approving any exit-4 decision, and revoking or rotating keys.
- **Cutting a release** (`cut-release.yml`, repository admins only) and
  deciding to promote it to Latest (`promote-release.yml`).

## Cutting a release

Releases are cut from GitHub, never by pushing a tag from a laptop. A
repository admin runs:

```bash
gh workflow run cut-release.yml --repo langstroth-ai/ligus -f version=X.Y.Z   # optional: -f ref=<branch or sha>
```

It tags `vX.Y.Z` only if `ci-ok` passed on that exact commit and every version
matches, then builds the release as a draft and publishes it as a prerelease
once every build has passed. With the notes committed as
`docs/release-notes/vX.Y.Z.md`, a repository admin then promotes it to Latest:

```bash
gh workflow run promote-release.yml --repo langstroth-ai/ligus -f version=X.Y.Z   # -f dry-run=true checks only
```

It checks the release, its assets, the Homebrew tap and the notes, makes the
release Latest, and opens the PR that moves `docs-site/docs/version.json`
forward.

Agents may prepare the version bump and the release notes and watch the runs,
but never push a `v*` tag, never publish a draft by hand, and never edit a
release by hand. Run `promote-release.yml` only when the owner asks for it in
so many words; never promote a release on your own initiative. The full flow
and the recovery steps are in
[docs/packaging.md](docs/packaging.md#how-to-cut-a-release).

## When something fails

- A serve or worker pod stuck in `Init`: its node can't sandbox agents.
  `kubectl -n NS logs POD -c sandbox-preflight`. Needs Linux 6.7+ with Landlock;
  `ligus-k8s preflight --probe --image ...` measures it (OrbStack: ABI 10).
- `helm template` errors are the chart refusing an unsafe or incomplete
  combination on purpose; fix the input it names.
- Reference: [docs-site/docs/operate/deploy/kubernetes.md](docs-site/docs/operate/deploy/kubernetes.md),
  [deploy/helm/ligus/README.md](deploy/helm/ligus/README.md),
  [deploy/ligus-serve/README.md](deploy/ligus-serve/README.md),
  [docs-site/docs/operate/deploy/with-an-agent.md](docs-site/docs/operate/deploy/with-an-agent.md).
- Tests for this tooling: `bash deploy/tests/agent-deploy-test.sh`.

# Deploying with an agent

You can hand the deployment of Ligus to a coding agent (Claude Code, Codex,
Copilot CLI, or another) working in a checkout of this repository. The
repository gives it three things:

- [`AGENTS.md`](https://github.com/langstroth-ai/ligus/blob/main/AGENTS.md) at the
  root: which deployment fits which goal, the exact commands, exit codes,
  the hard rules, and the steps only you can do. `CLAUDE.md` points to it.
- Two skills in the Agent Skills format:
  [`ligus-deploy-k8s`](https://github.com/langstroth-ai/ligus/blob/main/.claude/skills/ligus-deploy-k8s/SKILL.md)
  and
  [`ligus-deploy-local`](https://github.com/langstroth-ai/ligus/blob/main/.claude/skills/ligus-deploy-local/SKILL.md).
  Each is step by step, with checkpoints where the agent stops for your
  decision. Agents without skill support read the same files.
- Two non-interactive tools, `deploy/kubernetes/ligus-k8s` and
  `deploy/local/ligus-local`, that never prompt, are safe to rerun, never
  print a secret, and report with `--json`.

Ask, for example: *"Deploy Ligus on my OrbStack cluster as a trial"*, or
*"Set up ligus serve on this box for my workspace"*.

## What the agent does, and what you do

| Step | Agent | You |
|---|---|---|
| Choose the shape (local, trial cluster, team cluster) | proposes, from the table in AGENTS.md | confirm, and confirm the cluster context |
| Preflight: tools, cluster version, permissions, Pod Security, NetworkPolicy enforcement, StorageClass, Landlock on the nodes | runs `ligus-k8s preflight --probe` or `ligus-local preflight` | decide when a check fails (other nodes, another StorageClass) |
| Secrets | creates a throwaway workspace Secret for a trial | for a real workspace, write `room`, `key` (and a relay token) as files in a private directory; the agent only passes its path |
| Render, server-side dry run, install, wait | runs `ligus-k8s install --dry-run`, then without | review the dry-run summary for a team install |
| Verify | `ligus-k8s verify` / `ligus-local verify` | |
| Enroll, passkeys, invites | | `ligus enroll` with the invite, desktop sign-in, approving devices |
| Pair the phone, exposure, DNS and TLS | | yours |
| Tear down | `teardown` / `uninstall` | revoke the device from a desktop; decide on key rotation |

## The rules the agent follows

- It never handles secret values: not the workspace key or invite, relay or
  registration tokens, pairing links, model keys, database URLs or passwords.
  They go straight into Kubernetes Secrets or files, created by the tools or
  by you.
- It never turns off a fail-closed default to get an install through: the
  agent sandbox and its preflight, NetworkPolicies and egress limits, Pod
  Security `restricted`, TLS, ClusterIP-only serve. `ligus-k8s install`
  inspects the rendered manifest and refuses (exit 4) if any of these is
  off; that change is yours to make.
- Exit code 4 from either tool means "a human must decide or act"; the agent
  stops and shows you the `fix` and `next` lines.

## Kubernetes in short

```sh
K=deploy/kubernetes/ligus-k8s
$K preflight --context orbstack -n ligus-trial --release trial --probe --image ligus:dev --json
$K secrets workspace --throwaway --context orbstack -n ligus-trial --release trial
$K install --scenario trial --context orbstack -n ligus-trial --release trial \
  --image ligus:dev --dry-run --json
$K install ... (same flags, without --dry-run)
$K verify --context orbstack -n ligus-trial --release trial --json
$K teardown --context orbstack -n ligus-trial --release trial --delete-data --delete-secrets --delete-namespace
```

`ligus:dev` is an image you build from `deploy/container/Dockerfile` (the
`ligus` image isn't published). The relay chart and image come from
`ghcr.io/langstroth-ai` by default; `--relay-chart` and
`--relay-image-repository`/`--relay-image-tag` override them.

Scenarios: `trial` (relay and serve in one namespace, in-memory relay,
throwaway workspace), `team-in-cluster-relay` (relay installed separately in
this cluster) and `team-external-relay` (relay at an `https://` URL). Their
values files are in
[`deploy/kubernetes/scenarios`](https://github.com/langstroth-ai/ligus/tree/main/deploy/kubernetes/scenarios).
On OrbStack the probe measures Landlock ABI 10 and an enforced NetworkPolicy;
the full operator guide is [Kubernetes (preview)](kubernetes.md).

## This machine in short

```sh
L=deploy/local/ligus-local
$L preflight --role serve --data-dir ~/.ligus-serve --json
$L configure --role serve --data-dir ~/.ligus-serve --json   # then you run ligus enroll
$L start --role serve --data-dir ~/.ligus-serve --json
$L verify --role serve --data-dir ~/.ligus-serve --json
$L stop --role serve --data-dir ~/.ligus-serve
```

`--role worker` runs a worker instead; `--systemd` (Linux) installs a user
unit. On macOS, `--launchd` (optional) installs a LaunchAgent so the box
starts at login and is kept running; `stop --launchd` removes it. See
[Start at login on macOS](personal-fleet.md#start-at-login-on-macos). The desktop app itself is installed with Homebrew and opened by you:
see [Install with Homebrew](../../ligus/getting-started/homebrew.md). More on the
home box: [Personal fleet](personal-fleet.md).

## Shell completions

`deploy/completions/` has hand-written zsh and fish completions for both
tools (commands, flags, scenarios and, for `--context`, your kube contexts):

```sh
# zsh: a directory on $fpath, then compinit
mkdir -p ~/.zsh/completions && cp deploy/completions/_ligus-k8s deploy/completions/_ligus-local ~/.zsh/completions/
# fish
cp deploy/completions/ligus-k8s.fish deploy/completions/ligus-local.fish ~/.config/fish/completions/
```

They complete the tools by name, so put `deploy/kubernetes` and `deploy/local`
on your `PATH` (or link the scripts into it). The test suite checks they name
every command and flag the scripts take.

# Running untrusted PR tests in a container sandbox

Status: design only, not built. Written for the maintainer setup where a
headless host (`hive serve`) helps review pull requests from people the owner
doesn't know. Today agents may read a PR's diff through a read-only GitHub
MCP server, and the review skills forbid running PR code outside a sandbox.
This note describes the sandbox and what a "Run PR tests" action would take.

## Threat model

An external PR is arbitrary code. Its tests, `Makefile`, `conftest.py`,
`setup.py`/build backends, npm `postinstall` scripts, Go `//go:generate`
lines and test fixtures all execute with whatever the runner has. On the
host that would mean the owner's `gh` login (repo scope on an upstream where
the owner is an admin), the Hive identity and workspace key, the relay
tunnel, other checkouts, the Tailscale network (lab services, Prometheus,
model gateways) and the Docker socket. The sandbox must give the PR code
none of these, and must survive the code being actively hostile (fork bombs,
disk fill, crypto-mining, attempts to reach the network or escape).

The agent that asks for a run is also untrusted in the relevant sense: its
input is PR text. So the action is owner-approved (a proposal), never
something a turn can trigger by itself.

## The run

1. **Materialize the PR without running anything.** On the host, with the
   host's credentials but no PR code executing:
   `git fetch upstream pull/<n>/head:refs/hive/pr/<n>` in the fork checkout
   (fetch-only remote; `--no-recurse-submodules`; hooks never run on fetch),
   then `git archive refs/hive/pr/<n> | tar -x -C <scratch>/src` into a fresh
   scratch dir. `git archive` gives a plain tree: no `.git`, no hooks, no
   remotes, no credentials, no `.hive/` worktrees. Record the head SHA; the
   run is pinned to it (a force-push later doesn't change what ran).
   LFS and submodules are not fetched (a PR that needs them fails, loudly).

2. **Dependencies (optional, separate step).** Tests usually need
   dependencies the PR may change (`uv.lock`, `go.sum`, `package-lock.json`).
   Two options, chosen per recipe:
   - *Offline*: a prebuilt base image per area with dependencies from the
     **base branch** baked in (`kfp-sdk-test:<base-sha>`, built from the
     trusted `master` checkout). PRs that change dependencies are reported
     as "dependency change: not run offline".
   - *Restricted network for install only*: a first container on an
     egress-filtered network that can reach only package indexes (PyPI,
     proxy.golang.org, registry.npmjs.org) through an allowlisting proxy,
     writing into a volume. Install scripts still run here, so this
     container gets the same isolation as the test run; only its network
     differs. Then the test container runs with no network.

3. **Test container.** Roughly:

   ```sh
   docker run --rm \
     --network none \
     --read-only --tmpfs /tmp:rw,size=1g,mode=1777 \
     -v <scratch>/src:/src:ro \
     -v <scratch>/work:/work:rw \
     --workdir /work \
     --user 65534:65534 \
     --cap-drop ALL --security-opt no-new-privileges \
     --pids-limit 512 --memory 4g --memory-swap 4g --cpus 2 \
     --ulimit nofile=4096:4096 --ulimit fsize=2147483648 \
     --stop-timeout 5 \
     --label hive.sandbox=pr-<n>-<sha> \
     <base-image@digest> \
     sh -c 'cp -a /src/. /work/ && <recipe command>'
   ```

   - **No credentials mounted**: no `~/.config/gh`, `~/.ssh`, `~/.docker`,
     Hive data dir, env file or Docker socket; the environment is built from
     scratch (`--env-file` with only recipe variables, never `-e` passthrough
     of the host env).
   - **Read-only source**: the pristine tree is mounted read-only; tests run
     in a writable copy inside the container's own scratch, so the host
     never reads back anything the PR wrote except the captured output.
   - **Resource limits** as above plus a wall-clock limit enforced by the
     host (`docker kill` after N minutes, then `docker rm -f`), and the
     scratch dir on a size-limited filesystem (a loop-mounted image or an
     XFS project quota) so a disk-fill can't hurt the host.
   - **Stronger isolation** when available: rootless Docker or Podman, or
     gVisor (`--runtime runsc`), so a kernel-level escape needs more than a
     container breakout. On the SFF box today Docker runs rootful, so gVisor
     (or a throwaway VM via Firecracker or Kata) is the recommended upgrade
     before this runs unattended.

4. **Capture and report.** stdout/stderr (truncated, e.g. last 200 KB),
   exit code, duration, JUnit XML if the recipe produces it (copied out via
   `docker cp` from a known path, size-capped, parsed as data). The result
   is posted to the chat as a system note and attached to the review draft.
   Output is untrusted text: it is shown, never fed back as instructions,
   and terminal escape sequences are stripped.

5. **Cleanup.** Container removed, scratch dir deleted, `refs/hive/pr/<n>`
   deleted. Nothing from the run stays on the host.

## Recipes

A run executes a named recipe from a host-side file (never from the PR), so
a PR can't choose what command runs. For kubeflow/pipelines, from the repo's
own docs (`docs/agents/testing.md`, `sdk/CONTRIBUTING.md`):

| Recipe | Image (base-branch deps) | Command |
|---|---|---|
| `sdk-unit` | `kfp-sdk-test` (uv, Python, deps synced) | `uv run --offline pytest -q sdk/python/kfp` |
| `sdk-k8s` | same | `uv run --offline pytest -q sdk/python/test/kubernetes` |
| `backend-unit` | `kfp-go-test` (Go, module cache) | `GOFLAGS=-mod=mod GOPROXY=off go test ./backend/...` minus the cluster suites |
| `compiler-goldens` | `kfp-sdk-test` | the compiler golden tests only |

Cluster suites (Ginkgo end-to-end, Kind) are out of scope: they need a
cluster and network, which is a different trust problem.

## Wiring it into Hive

What it would take on the `hive serve` side:

- **Action**: `POST /v1/chats/{id}/pr-tests {pr, recipe}` creates a
  `RunPrTests` proposal (kind `action`, not `file_diff`) showing the PR
  number, head SHA, recipe and exact command. Only approval (owner, phone or
  desktop, role at least Contributor) starts a job; an agent can suggest it
  but never start it. The job reuses the clone-job machinery in
  `repos_api.rs` (`run_watched`, timeouts, progress events on the hub as
  `repo.prtest`, at most one at a time per host).
- **Config**: `LIGUS_PR_SANDBOX=docker|podman|off` (default off), recipes in
  `<data dir>/pr-test-recipes.toml` (name, image by digest, command, limits,
  network: none|deps-proxy), and an allowlist of upstream repos it may run
  for.
- **Pre-flight refusals**: Docker missing or rootful-without-gVisor when
  `LIGUS_PR_SANDBOX_REQUIRE_GVISOR=1`; the image not pinned by digest; the PR
  not on an allowed upstream; a recipe not in the file.
- **Result**: system note with pass/fail and a link to the full log on the
  host (`GET /v1/pr-tests/{job}/log`), plus a structured result an agent can
  read in its next turn as data.
- **Phone**: a "Run tests in sandbox" button on a PR row (from the GitHub
  MCP dashboard panels or a review chat) that files the proposal.
- **Tests**: an integration test with a fake container runtime behind a
  trait (like `GitHub` in `repos_api.rs`), plus an opt-in real-Docker test
  that checks the isolation claims from inside the container: no network
  (`getent hosts github.com` fails), no credentials (no `GH_TOKEN`, no
  `~/.config/gh`), read-only `/src`, PID and memory limits hit and reported,
  and the host scratch dir gone after the run.

## Not covered

Secrets-requiring tests, GPU tests, tests that need Kubernetes, and
reviewing the output for hidden exfiltration attempts (with no network there
is nowhere to send data; the log itself is treated as untrusted text).

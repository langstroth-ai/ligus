# The Linux agent sandbox

Every process Ligus starts for an agent turn on Linux (a CLI agent, a stdio
MCP server, `run_command`, a pre-flight test) runs in one of three sandbox
tiers. Ligus picks the strongest one the host supports when it starts, logs
the choice, shows it in **Settings → Permissions** when it isn't the
strongest, and `ligus doctor` prints it:

```console
$ ligus doctor
agent sandbox: landlock
  Landlock (ABI 8) + seccomp, without namespaces, because bubblewrap can't create a user namespace: AppArmor restricts unprivileged user namespaces on this host (kernel.apparmor_restrict_unprivileged_userns=1) and there is no AppArmor profile for bwrap (Ubuntu 24.04 ships none; 24.10 and later do)
  - to restore the bubblewrap tier, install the optional AppArmor profile for bwrap: https://docs.langstroth.ai/operate/ops/linux-sandbox/#apparmor-profile
  - …
```

| Tier | Needs | Agent turns |
|---|---|---|
| **bwrap** (strongest) | `bwrap` and unprivileged user namespaces | run |
| **landlock** | Linux 6.7+ with Landlock (ABI 4+) and seccomp | run, with the limits below |
| **unconfined** | nothing | refused at every tier, read-only included, along with `run_command`, pre-flight tests and stdio MCP servers; HTTP models answer without local tools |

In a Kubernetes Pod Security `restricted` pod (`RuntimeDefault` seccomp, no
capabilities) bubblewrap can't create its namespaces, so **Landlock is the only
tier there**; the Helm chart sets `LIGUS_SANDBOX=landlock` and
`LIGUS_REQUIRE_SANDBOX=1` (serve and workers exit instead of starting
unconfined) and checks the node with `ligus doctor --require-confined
--expect-tier landlock --turn-test`, which runs one real turn against a stub
model and exits non-zero if it isn't confined. See
[Kubernetes](../deploy/kubernetes.md#supported-nodes-the-agent-sandbox).

`LIGUS_SANDBOX=landlock` skips bubblewrap (to try the Landlock tier on a
host that has both), `LIGUS_SANDBOX=bwrap` refuses the Landlock fallback,
and `LIGUS_SANDBOX=off` runs unconfined.

## What each tier enforces

Both confined tiers enforce the same plan: the same denied reads, the same
writable paths, the same egress allow-list. They differ in how.

| | bwrap | landlock |
|---|---|---|
| Home | an empty tmpfs over your home; the [read-only allow-list](../../ligus/concepts/security.md#home-allow-list) and read-only grants are bound back in (shallowest first), then the worktree and the writable paths | no rule covers the home; the allow-list, the worktree, the turn's dirs and grants get rules of their own, carved around the denied paths below |
| Reads denied | Ligus's data dir (keys, `settings.json`, the database, `serve.token`), a `--token-file`, `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config/gcloud`, `~/.kube`, `~/.netrc`, `~/.git-credentials`, the keyring, `$XDG_RUNTIME_DIR` (session bus), hidden behind tmpfs mounts made after every bind, so a granted parent dir can't bring them back | the same, by Landlock rules: no rule covers a denied path, and the directories above one can be entered but not listed. Also the real CLI config dirs (`~/.claude`, `~/.claude.json`, `~/.codex`, …) when the CLI runs with its per-turn home, which holds the copies it needs |
| Writes | the worktree (minus `.git` and the protected paths), the turn's scratch dir and per-turn home, read-write grants, bind-mounted on a read-only `/`, and Landlock again | the same paths, by Landlock rules. The worktree's top-level directory itself has no write rule (any right on it would reach `.git`): existing top-level files and every subdirectory are writable, but nothing can be created, deleted or renamed directly at the top level |
| Network | a fresh network namespace; the only way out is the egress proxy, through a socket | Landlock TCP rules: connect only to the turn's egress proxy port, the Ligus MCP listener and allow-listed loopback ports; no TCP bind. seccomp refuses every other socket: UDP (so no DNS of its own, names resolve at the proxy), raw, packet, SCTP and MPTCP sockets, `listen`, TCP Fast Open |
| Unix sockets | `socket(AF_UNIX)` refused; known host sockets also masked | `socket(AF_UNIX)` refused; on Landlock ABI 6+ abstract sockets outside the sandbox are out of reach too |
| Processes | own PID, IPC and UTS namespaces | no namespaces; Landlock refuses `ptrace`-class access to processes outside the sandbox (their `/proc/<pid>/environ`, `mem`, `fd`); on ABI 6+ no signals to them |
| seccomp | `ptrace`, `process_vm_*`, `mount`, `bpf`, `perf_event_open`, `keyctl`, kernel modules, `kexec`, `setns`, new user namespaces, the x32 ABI; `io_uring` and `clone3` unavailable | the same, plus every namespace flag on `unshare`/`clone` |
| Also | `no_new_privs`, a new session, the env allow-list, dies with Ligus | the same |

## What the landlock tier leaves open

- **TCP is matched by port, not address.** Landlock's network rules name
  ports only. An agent can open a TCP connection to *any* host on the
  egress proxy's port (or the Ligus MCP listener's, or an allow-listed
  loopback port such as Ollama's 11434), so a prompt-injected agent could
  reach a server that listens on that port number. Under bubblewrap the
  network namespace has no route out at all.
- **Other processes are visible.** No PID namespace: `ps` shows the host's
  processes and their command lines. Their environment, memory and open
  files stay out of reach (Landlock's ptrace rules).
- **Signals** to your other processes (as your user) are possible on
  kernels before 6.12 (Landlock ABI below 6).
- **No terminals.** `/dev/pts` belongs to your real sessions, so the agent
  gets none (bubblewrap gives it a private one).
- **No local servers.** Without a network namespace nothing can contain a
  listener, so a CLI that runs its own helper server (agy's language
  server) fails here, even with its agent's
  [local-servers switch](../../ligus/concepts/security.md#local-servers) on.
  Under bubblewrap each turn has its own loopback, where it works.
- **The worktree's top level is fixed** (see Writes above). This is the
  limit agents notice most: a build or install that creates a new top-level
  directory (`cargo build` without `target/`, `npm install` without
  `node_modules/`) or a new top-level file fails with `Permission denied`.
  Create those directories before the turn, or install the AppArmor
  profile below to get the bubblewrap tier.
- **The Ligus MCP server listens on a loopback TCP port** for the turn
  (there's no namespace to hide a forwarder in). Every connection must
  still present the turn's credential.
- **`LIGUS_SANDBOX_UNIX_SOCKETS=allow`** lifts the unix-socket filter; below
  Landlock ABI 9 nothing then limits which socket paths an agent connects
  to (no masks without mounts), so leave it off in this tier.

## AppArmor profile {#apparmor-profile}

Ubuntu 24.04 sets `kernel.apparmor_restrict_unprivileged_userns=1` and
ships no AppArmor profile for `bwrap` (Ubuntu 24.10 and later ship
`bwrap-userns-restrict`), so bubblewrap fails with
`setting up uid map: Permission denied` and Ligus uses the landlock tier.
To get the bubblewrap tier back, install the optional profile from the
repository, [`deploy/linux/apparmor/ligus-bwrap`](https://github.com/langstroth-ai/ligus/blob/main/deploy/linux/apparmor/ligus-bwrap),
modelled on Ubuntu 24.10's: it lets `/usr/bin/bwrap` create user namespaces
and moves everything bwrap runs into a stacked profile without
capabilities.

```console
$ sudo install -m 0644 deploy/linux/apparmor/ligus-bwrap /etc/apparmor.d/ligus-bwrap
$ sudo apparmor_parser -r /etc/apparmor.d/ligus-bwrap
$ systemctl --user restart ligus-serve   # or restart the desktop / worker
$ ligus doctor
agent sandbox: bwrap
```

To remove it:

```console
$ sudo apparmor_parser -R /etc/apparmor.d/ligus-bwrap
$ sudo rm /etc/apparmor.d/ligus-bwrap
```

Trade-offs:

- **It applies to every user of `/usr/bin/bwrap`** on the host (Flatpak,
  thumbnailers, other sandboxes), not only Ligus: they can create user
  namespaces through bwrap again, as on a distro without the restriction or
  with Ubuntu's own profile. Processes bwrap starts get no capabilities, not
  even inside their own namespace, and can't create further namespaces.
- **Don't install it next to another bwrap profile** (Ubuntu 24.10+, or one
  you wrote): check `sudo aa-status | grep bwrap` first.
- It needs AppArmor 4.0 or later (`apparmor_parser --version`).
- Ligus never installs it, and never asks for root.

Alternatives: setting `kernel.apparmor_restrict_unprivileged_userns=0`
lifts the restriction for every program on the host (a much wider change),
and the [container image](../deploy/kubernetes.md) ships `bwrap` for
hosts where Ligus runs in a container.

## Setuid bubblewrap and `NoNewPrivileges` {#setuid-bwrap}

The shipped systemd unit (`deploy/ligus-serve/ligus-serve.service`) sets
`NoNewPrivileges=yes`. Most distros ship a plain `bwrap` that uses
unprivileged user namespaces, and that works under it. Where unprivileged
user namespaces are off, a distro ships `bwrap` **setuid root** instead, and
under `NoNewPrivileges=yes` it can't gain root: the bubblewrap tier is lost
(Ligus falls back to the Landlock tier, or refuses every turn without
Landlock ABI 4).

`ligus-local start --systemd` checks this: when `bwrap` is setuid and
`unshare -U true` fails, it writes `NoNewPrivileges=no`, with the reason as a
comment in the unit, and reports a `systemd.nnp` warning. Agent processes
still run with `no_new_privs` (bubblewrap and Ligus's sandbox helper set it
for every child); only the `ligus` process itself loses the flag. To keep
`NoNewPrivileges=yes`, turn on unprivileged user namespaces (or install a
non-setuid `bwrap`) and start again. `ligus-local unit --role serve
--data-dir DIR` prints the unit it would install. If you install the unit by
hand on such a host, make the same choice yourself.

## Checking a host

- `ligus doctor` prints the tier, why, and what it leaves open.
- The desktop log and `ligus serve`'s log (journald under systemd) carry
  the same lines at startup, target `sandbox`.
- The sandbox tests run under whichever tier the host has, and print
  `SKIPPED <test>: …` when there is none; set `LIGUS_REQUIRE_SANDBOX=1` in
  CI to make a missing sandbox a failure.

# `ligus` CLI reference

`ligus` is the headless Ligus client: the same runtime as the desktop app
(`hive-core` + `hive-runtime`) wired to flags and environment variables
instead of a window. It runs workers, the phone's home host
(`ligus serve`) and scripts. Install it with
`brew install langstroth-ai/ligus/ligus` (macOS and Linux) or build the container
image in `deploy/container/`.

This page is checked against `crates/hive-cli/src/main.rs` for 0.1.0. The
built-in help is `ligus --help` (also `-h`, `help`, or `--help` after any
command); it prints to stderr and exits with status 2.

## Global behaviour

- **Data dir.** Every command works on one data dir: `LIGUS_DATA_DIR`, else
  `~/.ligus`, else `./ligus-data` when `HOME` is unset. A data dir that
  holds (or links to) data Ligus did not create is refused. The dir is one
  Ligus **device**: its
  identity (`identity.json`, `keys/`), its event log (`hive.db`) and, for
  `ligus serve`, the phone credentials. Never run two processes that write
  to one data dir.
- **Connection.** `LIGUS_RELAY_URL`, `LIGUS_RELAY_ACCESS_TOKEN`,
  `LIGUS_WORKSPACE` (default `default`) and `LIGUS_WORKSPACE_KEY` override
  what `ligus enroll` saved in `<data dir>/config.toml`.
- **First run** of any command creates a fresh identity in the data dir
  (named `hive-cli`).
- **Errors** print `error: …` and exit 1.
- Flags are matched anywhere after the command; values follow the flag as
  the next argument (`--label box`, not `--label=box`).

## Version and identity

| Command | What it does |
|---|---|
| `ligus --version` (`-V`, `version`) | prints `ligus <version>` |
| `ligus whoami` | prints account id, device id, name, data dir and room |
| `ligus doctor` | prints this device's agent sandbox tier (`seatbelt`, `bwrap`, `landlock` or `unconfined`), why, and what it leaves open; see [The Linux agent sandbox](../../operate/ops/linux-sandbox.md) |

```console
$ ligus whoami
account : 78616e73-8a66-4c01-96ee-169f0fce430d
device  : df2bb74f-592e-475d-a1ee-ebd4068e1123
name    : hive-cli
data dir: /home/you/.ligus
room    : default
```

## Joining a workspace

| Command | What it does |
|---|---|
| `ligus enroll <hivews1:…> [--token <hrt1-token>]` | decodes a workspace invite (relay URL, room, E2EE key) and saves it, plus the optional relay access token, to `<data dir>/config.toml` |
| `ligus join <invite-code>` | redeems a **relay** invite code, enrolling this box's identity in the workspace at the invite's role. Needs a relay configured first |
| `ligus sync [--watch]` | one push + pull round with the relay (paging until caught up); `--watch` repeats every 3 s. Prints `pushed N, pulled M` |

```console
$ ligus enroll "hivews1:eyJy…"
Enrolled workspace 'acme' at https://relay.example.com.
Saved to /home/you/.ligus/config.toml.
No access token set — for a membership-enforcing relay, add one:
  ligus enroll <invite> --token <hrt1-token>   (or export LIGUS_RELAY_ACCESS_TOKEN)
Next: `ligus sync` then `ligus worker` to bring the agent online.
```

!!! warning "config.toml holds the workspace key"
    `ligus enroll` writes the E2EE key in clear to `config.toml`. Keep the
    data dir private (`chmod 700`), as with the key files next to it.

## Signing in to an organization's relay

For a relay behind [langstroth-auth](../../operate/deploy/ligus-auth.md):

| Command | What it does |
|---|---|
| `ligus auth login <url> [--label NAME]` | device-code sign-in at `https://auth.example.com/t/<tenant>`: prints a code to enter in a browser, then saves the session to `<data dir>/ligus-auth.json` (0600) |
| `ligus auth status` | the tenant, subject, granted workspaces and access-token lifetime (never the tokens) |
| `ligus auth logout` | revokes the session at langstroth-auth and deletes the file |

While a session exists, every `ligus` command uses its short-lived relay
token and refreshes it in the background (the refresh token only works with
this box's device key). `LIGUS_RELAY_ACCESS_TOKEN`, when set, takes
precedence. Signing in opens the relay only: joining a workspace still takes
an invite and an Admin.

## Chats and messages

| Command | What it does |
|---|---|
| `ligus chats` | lists chats in the local store |
| `ligus new [title…]` | creates a chat, prints its id |
| `ligus tail <chat-id> [--watch]` | prints a transcript; `--watch` follows it |
| `ligus send <chat-id> <message…> [--push]` | posts a signed message; `--push` syncs it out at once |
| `ligus proposals` | lists proposals with status and votes |
| `ligus queue` | unanswered agent mentions and whether each agent's host is online |

## Runtimes and agents

| Command | What it does |
|---|---|
| `ligus runtimes` | lists workspace-owned runtimes synced onto this box |
| `ligus add-runtime <id> <name> <provider> <model> [--endpoint <url>] [--secret-ref <name> \| --secret <value>]` | defines (or replaces by id) a workspace runtime. Not in `--help` |
| `ligus rm-runtime <id>` | removes a workspace runtime. Not in `--help` |
| `ligus setup [--apply]` | detects what this box can reach (API keys in env, Ollama on `127.0.0.1:11434`, `claude`, `codex`, `agy`, `copilot`); `--apply` defines the keyless ones as workspace runtimes |
| `ligus config export` | prints the workspace runtimes as TOML. Inline secrets are never exported; they become a `secretRef` |
| `ligus config import <file.toml>` | adds or replaces runtimes from that TOML (by id) |
| `ligus agents` | lists workspace agents. Not in `--help` |
| `ligus add-agent <name> <runtime-id> [--host <id>] [--role <role>]` | defines an agent. Not in `--help` |
| `ligus hosts` | lists workspace hosts (devices and workers) |
| `ligus register-worker [--label <name>]` | registers this box as a worker host, prints its host id |
| `ligus set-agent-host <agent> <host-id>` | binds an agent to a host |
| `ligus agent [name] [--runtime <id>]` | development responder: answers new un-mentioned or `@primary` messages. Refuses to run unless `LIGUS_ALLOW_PERSONAL_AGENT` is set; use `ligus worker` in production |

`<provider>` is one of `anthropic`, `openai`, `openrouter`, `ollama`,
`custom` (any OpenAI-compatible server), `claude-code`, `codex`,
`antigravity` (or `agy`) and `copilot`.

Credentials for `add-runtime`:

- `--secret-ref <name>`: the key stays off the shared log; each host that runs
  the agent sets `LIGUS_WS_SECRET_<name>`.
- `--secret <value>`: the key travels **E2EE on the workspace config log**, so
  every member's device holds it. Prefer `--secret-ref`.
- neither: keyless (local models, subscription CLIs).

## The worker daemon

```sh
ligus worker [--label <name>]
```

Registers this box as a host (label: `--label`, else `$HOSTNAME`, else
`worker`), syncs, and every 3 s answers queued mentions for the agents bound
to it, backlog included. It publishes a liveness beat on the relay's presence
channel, backs off on relay errors (6 s doubling to 120 s), finishes replies a
previous run was streaming when it stopped, and exits cleanly on SIGTERM or
Ctrl-C. Worker turns are read-only for Claude Code unless
`LIGUS_WORKER_PERMISSION_MODE` raises them. See
[Headless agents](../../operate/concepts/headless-agents.md).

```console
$ ligus worker --label prod-box
worker 5b1c… online — draining queued mentions for its agents. Ctrl-C to stop.
```

## The home host for the phone

| Command | What it does |
|---|---|
| `ligus serve [--listen ADDR] [--token-file PATH] [--label NAME] [--name NAME] [--public-url URL] [--ntfy-url URL] [--ntfy-topic T] [--pair] [--remote-relay URL] [--reg-token T] [--insecure-relay]` | sync + worker + the phone's HTTP/SSE API. `--pair` prints the pairing code at start. The other flags override the `LIGUS_SERVE_*`, `LIGUS_NTFY_*` and `LIGUS_REMOTE_*` variables |
| `ligus admit request [--label NAME]` | prints this box's join request (`ligusjoin1:…`) and fingerprint for an Admin to approve (desktop: workspace menu, **Admit a box…**) |
| `ligus admit close` | as an Admin's CLI identity, closes this workspace's settled box admissions (redeemed, or lapsed unredeemed) so no later or backdated redemption admits anyone; the desktop does it after every sync. Run `ligus sync` before and after |
| `ligus admit redeem <ligusadmit1:…>` / `ligus admit redeem -` / `ligus admit redeem --file PATH` | keeps a box admission (`ligusadmit1:…` sealed to this box, or a bearer `hivews1:…`, which is read only from a file or standard input, never the command line); the next `ligus serve` or `ligus worker` joins as this box's own service member. `LIGUS_ADMISSION_FILE` does the same at start-up |
| `ligus admit issue --recovery-words-file PATH [--ttl 1h] [--request ligusjoin1:…] [--out PATH]` | the headless Admin path: issues a box admission from your own CLI identity, confirmed with your recovery key; sealed to the box when `--request` is given. `--out` writes the code owner-only (0600) into an existing directory, which it leaves as it is |
| `ligus recovery-key create --out PATH` | registers a recovery key for this CLI identity (a CLI founder's first credential) and writes its 24 words to `PATH` (owner-only, 0600; the directory must exist and is left as it is) before registering the key. Never on a box |

`ligus serve enroll` was removed in 0.1 (a box is never a person's device):
it stops with a migration error. See
[Admitting a box](../../operate/ops/admitting-a-box.md).
| `ligus pair [--listen ADDR] [--token-file PATH] [--public-url URL] [--relay-only] [--remote-relay URL] [--reg-token T] [--insecure-relay]` | prints the phone pairing QR code and link. Reads the server's token; never creates one |
| `ligus remote enable --relay URL [--reg-token T] [--insecure-relay]` | turns on phone access through a relay tunnel (creates host keys, writes `tunnel/relay.json`) |
| `ligus remote status` (or `ligus remote`) | relay, host id, the running server's link state, phone counts |
| `ligus remote disable` | stops using the relay; keys and phones are kept |
| `ligus remote reset` | new host keys and host id; every phone must pair again |
| `ligus phones list` (or `ligus phones`) | phones paired over the relay |
| `ligus phones revoke <pid\|name> [--keep-token] [--token-file PATH]` | revokes one phone; also rotates `serve.token` unless `--keep-token` |
| `ligus phones certify <ligusphone1:code> \| --code-file PATH --recovery-words-file PATH --fingerprint XXXX-XXXX-XXXX-XXXX-XXXX` | "Add this phone" without a desktop: certifies the phone's key (the code its app shows) as one of this identity's devices, at the phone tier, confirmed with the recovery key. The fingerprint (on the phone, Settings > This phone) is required: a code whose key doesn't match is refused. Without `--fingerprint`, at a terminal it shows the fingerprint and asks you to confirm it; anywhere else it refuses. Never from a box; sync to publish |
| `ligus bind-repo <name\|path> [--chat <id>]` / `ligus bind-repo --clear [--chat <id>]` | binds a checkout for agent file tools (workspace default, or one chat) |
| `ligus repos` | shows the repos dir and the bindings |
| `ligus dashboard check [--file PATH]` | validates `dashboard.toml` and evaluates it once |
| `ligus dashboard default` | prints the generated default dashboard as TOML |
| `ligus dashboard metrics` | lists the built-in Ligus metrics panels can show |
| `ligus trust list \| enable <digest> \| enable-all [--repo PATH]` | the runtimes and MCP servers a repository ships, and which this host trusts ([Trust this workspace](../concepts/security.md#workspace-trust)); `enable` takes a digest prefix |
| `ligus facts export --jsonl [--since C]` | this host's content-free facts as `ligus.facts.v1` JSON lines (measurements, guard signals, and state facts from its projection), for external observability tools; the last line carries the cursor to pass as the next `--since` (unix milliseconds also work) ([Telemetry](../concepts/security.md#facts)) |

```console
$ ligus serve --listen 127.0.0.1:8787 --label home-box
ligus serve: listening on http://127.0.0.1:8787 — workspace 'acme', host 63f2… ('home-box'). Ctrl-C to stop.
Run `ligus pair` to show the phone pairing code.
```

See [Remote access for your phone](../../operate/networking/remote-access.md),
[Running ligus serve as your own device](../../operate/ops/serve-box.md) and the
[serve API reference](../../operate/reference/serve-api.md).

## Environment variables

Every variable the CLI reads is in [Environment variables](../../operate/reference/env-vars.md).

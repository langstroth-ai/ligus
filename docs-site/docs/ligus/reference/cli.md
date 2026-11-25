# `ligus` CLI reference

`ligus` is the headless Ligus client: the same runtime as the desktop app
(`ligus-core` + `ligus-runtime`) wired to flags and environment variables
instead of a window. It runs workers, the phone's home host
(`ligus serve`) and scripts. Install it with
`brew install langstroth-ai/ligus/ligus` (macOS and Linux) or build the container
image in `deploy/container/`.

This page is checked against `crates/ligus-cli/src` for 0.2.0; a test fails
when a command is missing here.

## Built-in help {#help}

The CLI documents itself, so a person or a coding agent can find and use
every command without this site:

| Command | What it prints |
|---|---|
| `ligus help` (or `ligus --help`, `-h`) | every command, by section, and the topics |
| `ligus help <command> [<subcommand>]`, or `ligus <command> [<subcommand>] --help` | that command's reference in the section order every Ligus tool uses: NAME (with what it does), USAGE, COMMANDS (its subcommands), FLAGS (each with its default), EXAMPLES, ENVIRONMENT (the variables it reads), EXIT CODES and SEE ALSO (related commands, the desktop menu path or the deploy script that does the same) |
| `ligus help --json` | every command (usage, flags, environment, examples, exit codes, see also) and topic as one `ligus.help.v1` document, for agents and completion scripts |
| `ligus help <topic>` | a task guide: the exact commands in order, each step with a command that checks it |

Topics:

| Topic | Guide |
|---|---|
| `quickstart` | join a workspace from this machine with its invite and post a message |
| `box` | an always-on `ligus serve` box: install, admit (`ligus admit request`, an Admin approves, `ligus admit redeem`), serve, pair; with `deploy/local/ligus-local` and a systemd unit or LaunchAgent |
| `worker` | a worker host that answers mentions for the agents bound to it |
| `phone` | pairing, **Add a phone** by scanning the desktop's code, and pasting the phone's code instead |
| `remote` | reaching the box from the phone through a relay tunnel |
| `kubernetes` | one workspace on a cluster with `deploy/kubernetes/ligus-k8s` |
| `repos` | workspace repositories, this host's checkouts, code hosts |
| `agents` | runtimes, agents, hosts and the access controls |
| `security` | the data dir, the secret backends, `LIGUS_DATA_DIR`, `LIGUS_SECRET_BACKEND` |
| `env` | every `LIGUS_*` variable the CLI reads (a test checks it against the code) |
| `troubleshooting` | the common errors and their fixes |
| `exit-codes` | the exit codes every Ligus tool shares (0 to 7), and `--legacy-exit-codes` ([CLI conventions](../../operate/reference/cli-conventions.md#exit-codes)) |
| `checks` | the `ligus.checks.v1` report `ligus doctor --json`, `langstroth-relay check --json` and the deploy scripts print ([CLI conventions](../../operate/reference/cli-conventions.md)) |
| `setup` | the guided setup flows (`ligus setup agent`, `tool`, `folder`, `box`, `phone`, `invite`, `relay`): flags, `--json`, exit codes and who may run which |
| `agents-guide` (also `AGENTS`) | a one-shot playbook for coding agents: set up a box, add an agent, deploy on Kubernetes, check health |

A name that is both a topic and a command (`agents`, `repos`, `remote`,
`worker`) prints the guide, then the command's reference; `ligus <command>
--help` prints only the reference. Help never opens the data dir. It prints to
stdout and exits 0; an unknown command or topic exits 2.

## Exit codes {#exit-codes}

One table for every Ligus tool: `ligus`, `langstroth-relay` and the deploy
scripts ([CLI conventions](../../operate/reference/cli-conventions.md#exit-codes);
`ligus help exit-codes`).

| Code | Meaning | When `ligus` uses it |
|---|---|---|
| 0 | done (warnings allowed) | success, help included |
| 1 | failed: the message (or `checks[].fix`) says why | an error (`ligus: …` on stderr); a failed `ligus doctor` or `ligus gateway-check` check |
| 2 | usage error | an unknown command or subcommand, a missing or bad argument such as a malformed chat id (the message says which help to read); also a model gateways file (`LIGUS_MODEL_GATEWAYS_FILE`) that can't be used |
| 3 | a precondition you can fix is missing | `ligus serve` or `ligus worker` with `LIGUS_REQUIRE_SANDBOX=1` on a host without an agent sandbox; the relay rejected the access token; a `ligus setup` flow found a harness not installed or signed in, or no agent sandbox |
| 4 | a person must decide or act | `ligus setup box` waiting for an Admin's passkey approval, `ligus setup phone` (run `ligus pair` yourself), `ligus setup invite` (create it on the desktop) |
| 5 | not found | an unknown chat (`ligus tail`, `ligus send`) |
| 6 | refused by workspace rules | the workspace refused the action (a role, an agent's access, a relay roster) |
| 7 | offline, the relay unreachable, or timed out | the relay couldn't be reached, or asked to wait (a rate limit) |

Before 0.2, `ligus doctor` exited 3 (`--require-confined` or `--turn-test`
without an agent sandbox), 4 (`--expect-tier`: another tier) or 5
(`--turn-test` failed), and `ligus gateway-check` exited 6 when a check
failed. Both now exit 1 with the failing check's id in the output;
`--legacy-exit-codes` keeps the old codes for one release.

`ligus` with no command prints the command list to stderr and exits 2. An
unknown command exits 2 without creating a data dir.

## Output for scripts and agents {#json}

Read and status commands take `--json` and then print one JSON document on
stdout: `ligus doctor` and `ligus setup [<flow>]` (a [`ligus.checks.v1`](../../operate/reference/cli-conventions.md) report), `ligus status`, `ligus whoami`, `ligus chats`, `ligus runtimes`,
`ligus agents`, `ligus hosts`, `ligus repos list`, `ligus remote status`,
`ligus phones list`, `ligus memory list|search|show|proposals` and
`ligus task-state`. `ligus facts export --jsonl` prints JSON lines. No
command prompts when standard input isn't a terminal: the one question
(`ligus phones certify` asking you to confirm the fingerprint) has a flag
(`--fingerprint`), and values read from standard input (`-`, the memory body,
`ligus forge token`) are read from a pipe.

## Global behaviour

- **Data dir.** Every command works on one data dir: `LIGUS_DATA_DIR`, else
  `~/.ligus`, else `./ligus-data` when `HOME` is unset. A data dir that
  holds (or links to) data Ligus did not create is refused. The dir is one
  Ligus **device**: its
  identity (`identity.json`, `keys/`), its event log (`ligus.db`) and, for
  `ligus serve`, the phone credentials. Never run two processes that write
  to one data dir.
- **Connection.** `LIGUS_RELAY_URL`, `LIGUS_RELAY_ACCESS_TOKEN`,
  `LIGUS_WORKSPACE` (default `default`) and `LIGUS_WORKSPACE_KEY` override
  what `ligus enroll` (a person's CLI) or `ligus admit redeem` (a box) saved
  in `<data dir>/config.toml`.
- **First run** of any command creates a fresh identity in the data dir,
  named after `LIGUS_SERVE_LABEL`, else the host name. `ligus app`,
  `ligus help` and `--help` never open the data dir, the config or the
  secret store; `ligus status` and `ligus facts export` never create an
  identity.
- **Errors** print `ligus: <what>` on stderr, then `why:` and `fix:` lines
  when known (a usage error's fix names the help to read); with `--json` the
  error is also one `ligus.error.v1` document on stdout
  ([errors](../../operate/reference/cli-conventions.md#errors), [exit codes](#exit-codes)).
- Flags are matched anywhere after the command; a value follows the flag as
  the next argument or after an equals sign (`--label box` or
  `--label=box`).
- Every command also takes `-o json|text` (`-o json` is `--json`; `-o jsonl`
  is `--jsonl` for `ligus facts export`), `-q` (`ligus doctor` prints only the
  checks that need attention) and `--color auto|always|never` (colour only on
  a terminal, honouring `NO_COLOR` and `CLICOLOR_FORCE`; JSON is never
  coloured). `ligus` never asks for a confirmation, so there is no `--yes`.

## Version and identity

| Command | What it does |
|---|---|
| `ligus --version` (`-V`, `version`) | prints `ligus <version>` |
| `ligus status [--json]` | one summary of this data dir without changing it: the identity (and whether it is a box), the workspace and relay, whether an access token, the workspace key and a local copy are present, membership and device standing, a kept admission, the serve token, remote access and the running server's relay link, the agent sandbox tier, and the next step |
| `ligus whoami [--json]` | prints account id, device id, name, data dir and room |
| `ligus doctor [--json]` | runs every check this host can make and says how to fix what fails: the agent sandbox tier (`seatbelt`, `bwrap`, `landlock` or `unconfined`), why, and what it leaves open (see [The Linux agent sandbox](../../operate/ops/linux-sandbox.md)); whether the process is dumpable; the workspace's signed sandbox grants; the secret store; the workspace connection; the relay (one request, 10 seconds at most; a failure names `langstroth-relay check --url <relay>` to run on the relay host); the held events (events this device can't judge yet, which never stop sync: [held events](../../operate/security/threat-model.md#held-events)); and this host's model gateways. Exits 1 when a check fails. `--json` prints one [`ligus.checks.v1`](../../operate/reference/cli-conventions.md) report; the text form starts with `agent sandbox: <tier>` and its summary, then one line per check and a summary line |
| `ligus doctor [--require-confined] [--expect-tier <tier>] [--turn-test]` | the sandbox checks only, as a preflight that exits 1 when one fails: `--require-confined` without an agent sandbox, `--expect-tier` on any other tier, `--turn-test` runs one real headless agent turn against an in-process stub model that probes the sandbox (its findings are `turn test: …` lines). `--legacy-exit-codes` exits 3, 4 and 5 instead, as before 0.2 |
| `ligus doctor --sandbox [--runtime <name>] [--agent <id>]` | the sandbox checks only, then what an agent turn may read and write on this host (the home is denied by default), why, the shell rule, and the signed grants in force |
| `ligus doctor --gateways [--gateway <name>] [--model <id>]` | the model gateway checks only, the same ones `ligus gateway-check` runs, as checks |
| `ligus gateway-check [--gateway <name>] [--model <id>]` | proves this host's [model gateways](../../operate/deploy/model-gateways.md): a hosted-API turn and a sandboxed CLI child through the egress proxy's route |

```console
$ ligus whoami
account : 78616e73-8a66-4c01-96ee-169f0fce430d
device  : df2bb74f-592e-475d-a1ee-ebd4068e1123
name    : ligus-cli
data dir: /home/you/.ligus
room    : default
```

## Opening the desktop app

| Command | What it does |
|---|---|
| `ligus app [--json]` | opens the Ligus desktop app and returns at once. The app is started detached from the terminal, so closing the terminal leaves it running. It takes no arguments but `--json`: the desktop app has no command-line options or `ligus://` links |

How it finds the app:

- **macOS:** in this order, the bundle named by `LIGUS_APP`, then
  `/Applications/Ligus.app`, then `~/Applications/Ligus.app`, then, as a
  last resort, the copies Spotlight knows by bundle id `ai.ligus.desktop`
  (the one at the CLI's version, else the newest). A copy on a mounted disk
  image (under `/Volumes/`) or an App Translocation copy is never picked
  unless `LIGUS_APP` names it. The chosen bundle is opened by path
  (`open /Applications/Ligus.app`), not by bundle id: Launch Services would
  pick the highest `CFBundleVersion` it has seen, which can be an old test
  build on a mounted disk image. An app that is already running from that
  path is brought to the front. A `LIGUS_APP` that isn't an app bundle is
  an error.
- **Windows:** where the installer recorded it in the registry (the NSIS
  uninstall key's `InstallLocation` and `Software\ligus\Ligus`, or the MSI's
  `InstallDir`), then `%LOCALAPPDATA%\Ligus`, `%LOCALAPPDATA%\Programs\Ligus`,
  `%ProgramFiles%\Ligus` and `%ProgramFiles(x86)%\Ligus`, then
  `ligus-desktop.exe` on `PATH`.
- **Linux:** `ligus-desktop` on `PATH` (the deb and rpm packages), then the
  AppImage named by `LIGUS_APPIMAGE`, then the newest `Ligus*.AppImage` in
  `~/Applications` or `~/.local/bin`. It needs a graphical session: with
  neither `DISPLAY` nor `WAYLAND_DISPLAY` set (a `ligus serve` box, a
  container, an SSH login) it stops with an error and starts nothing.

On macOS, two more checks:

- When the app's version (its `CFBundleShortVersionString`) differs from
  the CLI's, a one-line note on stderr names both and the one to update
  (an older app: `brew upgrade --cask --greedy ligus-app`; an older CLI:
  `brew upgrade ligus`). The app opens anyway.
- When Ligus is already running from another path (a disk image, a build
  directory), `ligus app` prints `Ligus is already running from <path>;
  quit it to use /Applications/Ligus.app` on stderr and opens nothing,
  rather than switching to that copy without a word.

With `--json` it prints one `ligus.app.v1` object on stdout: `path` (the
app it chose), `version`, `cliVersion`, `versionMatches`, `launched`,
`alreadyRunning`, `runningFrom` (macOS: the other running copy) and `note`
(the version note). `version` is set on macOS only.

On Windows and Linux the desktop app has no single-instance handoff, so
when it is already running `ligus app` prints `Ligus is already running;
switch to its window.` and does not start a second copy.

When the app isn't found, `ligus app` lists where it looked, points to
<https://langstroth.ai/ligus/download> (on macOS also
`brew install --cask langstroth-ai/ligus/ligus-app`, and `LIGUS_APP` for
an app elsewhere) and exits 1.

## Secrets on the command line

A value on the command line leaks to `ps`, `/proc` and shell history. The
flags that carry a secret (`--secret`, `--reg-token`, `--token`) take `-`
(read the value from stdin) or `@FILE` (read it from a file), and print a
warning when the value itself is on the command line. Only one value per
command can come from stdin.

A secret file (`@FILE`, and `--recovery-words-file`) must be a regular file
that only its owner can read (`chmod 600`); anything else (a file others can
read, a directory, a FIFO) is refused before it is read. A link is followed:
the file it points to is the one checked. Recovery words and seeds are wiped
from memory once used.

## Joining a workspace

`ligus enroll` is for a person's own CLI device, joining as a new member (already in the workspace on your desktop? [link this computer](#devices) instead). A serve or worker box never
enrolls with an invite: it joins only through a box admission
(`ligus admit request`, an Admin approves it, `ligus admit redeem`; see
[The home host for the phone](#the-home-host-for-the-phone) and
`ligus help box`).

| Command | What it does |
|---|---|
| `ligus enroll - \| --file PATH [--token - \| @FILE]` | decodes a workspace invite (relay URL, room, E2EE key), piped (`-`) or from a file, and saves the relay and room to `<data dir>/config.toml` and the key and the optional relay access token to the secret store. An invite on the command line still works, with a warning. An invite whose workspace has no usable relay URL is refused |
| `ligus join <invite-code>` | redeems a **relay** invite code, adding this identity to the workspace's relay roster at the invite's role. Needs a relay configured first |
| `ligus sync [--watch]` | one push + pull round with the relay (paging until caught up); `--watch` repeats every 3 s. Prints `pushed N, pulled M` |

```console
$ ligus enroll --file ./invite
Enrolled workspace 'acme' at https://relay.example.com.
Saved to /home/you/.ligus/config.toml.
No access token set — for a membership-enforcing relay, add one:
  ligus enroll --file <invite-file> --token @<token-file>   (or export LIGUS_RELAY_ACCESS_TOKEN)
Next: `ligus sync`. (A serve or worker box doesn't enroll: it joins with `ligus admit request` and `ligus admit redeem`, see `ligus help box`.)
```

!!! note "Where the connection is kept"
    `config.toml` (owner-only) holds only the relay URL and the room. The
    workspace key, the relay access token and delivered epoch keys go to the
    secret store (`LIGUS_SECRET_BACKEND`, see `ligus help security`). A
    `config.toml` that still holds secrets in the clear has them removed;
    run `ligus enroll` again.

## Linking this computer to your devices {#devices}

An invite always makes a **new member**. If you are already in the workspace
on your desktop, link this computer as one of your devices instead: it then
acts as you, not as a second person with your name. See
[Add a computer](../features/linked-devices.md).

| Command | What it does |
|---|---|
| `ligus devices request [--label NAME] [--new-device] [--copy] [--json]` | prints this computer's link request (`liguslink1:…`, no secret) and remembers it as pending (15 minutes to approve, single use). Only the request goes to stdout, so `ligus devices request \| pbcopy` copies exactly it; the fingerprint, the time to approve by and the next step go to stderr. `--copy` copies it to the clipboard instead; `--json` prints one report whose `result` has `code`, `fingerprint`, `label`, `expiresAt` (unix ms) and `newDevice`. A data dir that was already connected asks with a new device key, switched to only when you redeem. A box can't: it joins with `ligus admit` |
| `ligus devices redeem [<ligusgrant1:code> \| - \| --code-file PATH] [--approver-fingerprint FP]` | opens the grant your desktop showed (sealed to this computer). With no code at a terminal it asks for a hidden paste, or reads the clipboard when you just press Enter (**Copy for lt** on the desktop copies the grant alone); a pasted whole command or line breaks around the code are fine, and the code is never echoed. It asks you to confirm who approved it: *Approved by Alex's desktop · fingerprint … · for acme. Is that your desktop? [y/N]* (or `--approver-fingerprint`; away from a terminal without it, exit 4). Then it saves the connection, pulls, checks your desktop's certificate for this computer is there (exit 7 until it has synced) and acts as you from then on |
| `ligus devices approve <liguslink1:request> \| --code-file PATH --fingerprint FP --recovery-words-file PATH [--out PATH]` | a person's own headless approval (prefer the desktop's My devices > Add a computer): certifies the computer as yours with your recovery key, syncs, and prints the grant (or writes it owner-only to `--out`) |
| `ligus devices list [--json]` | your devices in this workspace, whether this computer acts as you, and a link still waiting for the desktop's code (`ligus status` shows that too). Expired requests are cleaned up as you go |
| `ligus devices rename <device-id \| this> <name> [--json]` | gives one of your devices (or this one) a name every member sees; an empty name clears it. A linked computer renames only itself |
| `ligus profile set [--name NAME] [--avatar FILE \| --clear-avatar] [--json]` | publishes your name and/or picture (PNG, JPEG or WebP, 16 KB at most) as your profile in this workspace: every member sees it, in past content too. Only a computer holding your account key can; at most one change a minute and 20 a day |

```console
$ ligus devices request --copy
Copied this computer's link request (it holds no secret).
Fingerprint: 7F3A-91C0-77DE-0B14-5E62  (label "ligus on work-laptop")
On your desktop: Settings > Profile > My devices > Add a computer; paste the request, compare the fingerprint, confirm with your passkey (by 14:17).
Then here: ligus devices redeem   (it reads the desktop's code from the clipboard or a hidden paste)
$ ligus devices redeem
Paste the desktop's code (hidden) and press Enter, or just press Enter to read it from the clipboard:
Approved by Alex's desktop · fingerprint 2C41-88E0-1B9D-04AF-6E73 · for acme. Is that your desktop? [y/N] y
This computer is now one of Alex's devices in workspace 'acme' (approved by desktop 2C41-88E0-1B9D-04AF-6E73).
```

In a script, pipe the grant in and pin the desktop, so nothing secret is on
the command line: `pbpaste | ligus devices redeem - --approver-fingerprint 2C41-88E0-1B9D-04AF-6E73`.

A linked computer acts as you without your account key: a vote asks for your
passkey, and account changes (devices, phones, passkeys, keys, invites as
founder) stay on the desktop. It doesn't accept invites or become a box. A
gated relay asks it to sign in as you (`ligus auth login`); the relay token is
never in the codes.

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
| `ligus chats [--json]` | lists chats in the local store |
| `ligus new [title…]` | creates a chat, prints its id |
| `ligus tail <chat-id> [--watch]` | prints a transcript; `--watch` follows it |
| `ligus send <chat-id> <message…> [--push] [--wait [--timeout <duration>]]` | posts a signed message; `--push` syncs it out at once; `--wait` also pushes, then syncs until someone else answers in the chat and prints the answer (`--timeout`, default 5m, as `90s`, `5m` or `1h`; no answer by then exits 7) |
| `ligus proposals` | lists proposals with status and votes |
| `ligus queue` | unanswered agent mentions and whether each agent's host is online |

## Guided setup {#setup}

The same seven flows as the desktop's **Set up** commands ([Set up](../getting-started/set-up.md)).
Each checks first, writes only when every check passed, and is safe to run
again. `ligus help setup` is the guide; `ligus setup <flow> --help` lists a
flow's flags.

| Command | What it does |
|---|---|
| `ligus setup [--json]` | lists the flows and what each has done on this device, as a `ligus.checks.v1` report (`<flow>.status`) |
| `ligus setup agent --harness H --name NAME [--model M] [--endpoint URL \| --gateway NAME] [--test-turn]` | checks the harness is installed and signed in and the agent sandbox, reuses or defines a keyless workspace runtime (Admin), creates the agent (Contributor), optionally sends one test turn |
| `ligus setup tool --id ID (--command "PROG ARGS" \| --kind url --url URL [--token - \| @FILE]) [--reach all\|AGENT,...]` | starts the MCP server once under the agent sandbox and lists its tools, then saves it with which agents may reach it (`<data dir>/mcp-servers.toml`, `mcp-reach.json`); a token goes to the secret store |
| `ligus setup folder --path DIR [--name NAME] [--branch B]` | checks the checkout, matches (or, as an Admin, adds) the workspace repository, binds the folder |
| `ligus setup box [--label NAME] [--code - \| @FILE]` | on a box: prints its join request and stops (exit 4) until an Admin approves it with a passkey; then keeps the admission and checks `ligus serve` |
| `ligus setup phone` | checks the box can pair, then hands over to `ligus pair` in your own terminal (exit 4) |
| `ligus setup invite` | checks you can invite and lists invites and whether they were accepted; invites are created on the desktop (exit 4) |
| `ligus setup relay [--kind community \| --kind own --url URL] [--token - \| @FILE]` | checks the relay (https only, plain http only on this machine), switches this device to it, checks again |
| `ligus setup runtimes [--apply]` | the detection `ligus setup` did before the flows |

Every flow takes `--json` (one report: `command` is `setup <flow>`, plus
`flow`, `done`, `dryRun` and `results`) and `--dry-run` (checks and validates,
writes nothing). A secret flag takes `-` (stdin) or `@FILE` (owner-only) and is
refused on the command line.

## Runtimes and agents

| Command | What it does |
|---|---|
| `ligus runtimes [--json]` | lists workspace-owned runtimes synced onto this box (never a secret: a `secretRef` shows whether this host has `LIGUS_WS_SECRET_<ref>`) |
| `ligus add-runtime <id> <name> <provider> <model> [--endpoint <url>] [--secret-ref <name> \| --secret - \| --secret @FILE] [--dry-run]` | defines (or replaces by id) a workspace runtime. `--dry-run` checks the arguments and prints what it would define, signing and writing nothing (the same on `rm-runtime`, `add-agent` and `set-agent-host`; `add-agent --dry-run` also checks the runtime exists). Any other command refuses `--dry-run` with a usage error (exit 2) and runs nothing. |
| `ligus rm-runtime <id> [--dry-run]` | removes a workspace runtime. |
| `ligus setup runtimes [--apply]` | detects what this box can reach (API keys in env, Ollama on `127.0.0.1:11434`, `claude`, `codex`, `agy`, `copilot`); `--apply` defines the keyless ones as workspace runtimes. This was `ligus setup [--apply]`; `ligus setup --apply` still works for one release |
| `ligus config export` | prints the workspace runtimes as TOML. Inline secrets are never exported; they become a `secretRef` |
| `ligus config import <file.toml>` | adds or replaces runtimes from that TOML (by id) |
| `ligus agents [--json]` | lists workspace agents. |
| `ligus add-agent <name> <runtime-id> [--host <id>] [--role <role>] [--dry-run]` | defines an agent. |
| `ligus hosts [--json]` | lists workspace hosts (devices and workers) |
| `ligus register-worker [--label <name>]` | registers this box as a worker host, prints its host id |
| `ligus set-agent-host <agent> <host-id> [--dry-run]` | binds an agent to a host |
| `ligus agent [name] [--runtime <id>]` | development responder: answers new un-mentioned or `@primary` messages (in a shared room only its operator's, unless `LIGUS_AGENT_OPEN_TO_MEMBERS=1`). Refuses to run unless `LIGUS_ALLOW_PERSONAL_AGENT` is set; use `ligus worker` in production |

`<provider>` is one of `anthropic`, `openai`, `openrouter`, `ollama`,
`custom` (any OpenAI-compatible server), `claude-code`, `codex`,
`antigravity` (or `agy`) and `copilot`.

Credentials for `add-runtime`:

- `--secret-ref <name>`: the key stays off the shared log; each host that runs
  the agent sets `LIGUS_WS_SECRET_<name>`.
- `--secret - | @FILE`: the key (read from stdin, or from an owner-only
  file) travels **E2EE on the workspace config log**, so every member's
  device holds it. Prefer `--secret-ref`.
- neither: keyless (local models, subscription CLIs).

## The worker daemon

```sh
ligus worker [--label <name>] [--health-listen <addr>]
```

Registers this box as a host (label: `--label`, else `$HOSTNAME`, else
`worker`), syncs, and every 3 s answers queued mentions for the agents bound
to it, backlog included. It publishes a liveness beat on the relay's presence
channel, backs off on relay errors (6 s doubling to 120 s), finishes replies a
previous run was streaming when it stopped, and exits cleanly on SIGTERM or
Ctrl-C. Worker turns are read-only for Claude Code unless
`LIGUS_WORKER_PERMISSION_MODE` raises them. `--health-listen` (or
`LIGUS_WORKER_HEALTH_LISTEN`) serves `GET /v1/health` and `/v1/ready` on that
address. See
[Headless agents](../../operate/concepts/headless-agents.md).

```console
$ ligus worker --label prod-box
worker 5b1c… online — draining queued mentions for its agents. Ctrl-C to stop.
```

## The home host for the phone

| Command | What it does |
|---|---|
| `ligus serve [--listen ADDR] [--token-file PATH] [--label NAME] [--name NAME] [--public-url URL] [--ntfy-url URL] [--ntfy-topic T] [--pair] [--remote-relay URL] [--reg-token - \| @FILE] [--insecure-relay]` | sync + worker + the phone's HTTP/SSE API. `--pair` prints the pairing code at start. The other flags override the `LIGUS_SERVE_*`, `LIGUS_NTFY_*` and `LIGUS_REMOTE_*` variables |
| `ligus admit request [--label NAME]` | prints this box's join request (`ligusjoin1:…`) and fingerprint for an Admin to approve (desktop: workspace menu, **Admit a box…**) |
| `ligus admit close` | as an Admin's CLI identity, closes this workspace's settled box admissions (redeemed, or lapsed unredeemed) so no later or backdated redemption admits anyone; the desktop does it after every sync. Run `ligus sync` before and after |
| `ligus admit redeem <ligusadmit1:…>` / `ligus admit redeem -` / `ligus admit redeem --file PATH` | keeps a box admission (`ligusadmit1:…` sealed to this box, or a bearer `hivews1:…`, which is read only from a file or standard input, never the command line); the next `ligus serve` or `ligus worker` joins as this box's own service member. `LIGUS_ADMISSION_FILE` does the same at start-up |
| `ligus admit issue --recovery-words-file PATH [--ttl 1h] [--request ligusjoin1:…] [--out PATH]` | the headless Admin path: issues a box admission from your own CLI identity, confirmed with your recovery key; sealed to the box when `--request` is given. `--out` writes the code owner-only (0600) into an existing directory, which it leaves as it is |
| `ligus recovery-key create --out PATH` | registers a recovery key for this CLI identity (a CLI founder's first credential) and writes its 24 words to `PATH` (owner-only, 0600; the directory must exist and is left as it is) before registering the key. Never on a box |

| `ligus pair [--listen ADDR] [--token-file PATH] [--public-url URL] [--relay-only] [--remote-relay URL] [--reg-token - \| @FILE] [--insecure-relay]` | prints the phone pairing QR code and link. Reads the server's token; never creates one |
| `ligus remote enable --relay URL [--reg-token - \| @FILE] [--insecure-relay]` | turns on phone access through a relay tunnel (creates host keys, writes `tunnel/relay.json`) |
| `ligus remote status [--json]` (or `ligus remote`) | relay, host id, the running server's link state, phone counts |
| `ligus remote disable` | stops using the relay; keys and phones are kept |
| `ligus remote push-test [--phone <pid\|name>]` | sends a test notification to paired phones (or one) and shows each hop |
| `ligus remote reset` | new host keys and host id; every phone must pair again |
| `ligus phones list [--json]` (or `ligus phones`) | phones paired over the relay, and revocations in progress |
| `ligus phones revoke <pid\|name> [--keep-token] [--token-file PATH]` | revokes one phone; also rotates `serve.token` unless `--keep-token` |
| `ligus phones certify <ligusphone1:code> \| --code-file PATH --recovery-words-file PATH --fingerprint XXXX-XXXX-XXXX-XXXX-XXXX` | "Add a phone" without a desktop: certifies the phone's key (the code its app shows) as one of this identity's devices, at the phone tier, confirmed with the recovery key. The fingerprint (on the phone, Settings > This phone > Can't scan? Show this phone's code) is required: a code whose key doesn't match is refused. Without `--fingerprint`, at a terminal it shows the fingerprint and asks you to confirm it; anywhere else it refuses. Never from a box; sync to publish |
| `ligus bind-repo <name\|path> [--chat <id>]` / `ligus bind-repo --clear [--chat <id>]` | binds a checkout for agent file tools (workspace default, or one chat) |
| `ligus forge list` | this device's code-host connections ([Code hosts](../features/code-hosts.md)) and which tokens are present |
| `ligus forge add <kind> <https://base> [--auth token\|oauth\|gh\|none] [--client-id ID] [--internal]` | adds a connection; `--internal` allows a loopback, private or link-local server |
| `ligus forge token <id> <read\|write>` | stores a token read from standard input in the OS secret store |
| `ligus forge login <id>` | device sign-in (GitHub App, GitLab application) |
| `ligus forge test <id>` | who the connection is signed in as |
| `ligus forge remove <id>` | removes the connection and its tokens |
| `ligus forge git-credential <id> <get\|store\|erase>` | git's credential helper for Ligus's own pushes (git runs it) |
| `ligus repos list [--json]` (or `ligus repos`) | the workspace's repositories, this host's checkouts of them, the repos dir and the bindings |
| `ligus repos add <remote\|canonical\|path> [--name N] [--kind K] [--branch B]` | adds a workspace repository (Admin, with presence); a path also binds it here |
| `ligus repos remove <repo>` | removes a workspace repository (Admin, with presence) |
| `ligus repos rename <repo> <name> [--branch B]` | renames a workspace repository |
| `ligus repos bind <repo> <path>` | this host's checkout of a repository (never synced) |
| `ligus repos confirm <repo> <path>` | confirms a checkout `scan` found |
| `ligus repos unbind <repo> [path]` | forgets this host's checkout (the files stay) |
| `ligus repos scan` | finds checkouts under the repos dir and the scan roots; found ones need `confirm` |
| `ligus repos roots [add\|remove <path>]` | the folders `scan` searches |
| `ligus repos alias <ssh-host> <host> [--remove]` | this host's own SSH host alias, for identity only (never synced) |
| `ligus repos aliases` | the workspace's synced repository and host aliases, and which this host accepted |
| `ligus repos repo-alias <repo> [name…]` | replaces a repository's synced aliases; no names clears them (Admin, with presence) |
| `ligus repos host-alias <alias-host> <repository-host>` / `host-alias <alias-host> --remove` | a synced host alias (Admin, with presence) |
| `ligus repos accept <alias> [--revoke]` | uses a synced alias for identity on this host (device-local) |
| `ligus repos chat <chat-id> <repo\|none> [--branch B]` | sets a chat's repository |
| `ligus repos coverage on\|off\|default` | whether this host tells members which repositories it has checked out |
| `ligus repos takeover <path>` | manages a checkout whose managing host is gone |
| `ligus dashboard check [--file PATH]` | validates `dashboard.toml` and evaluates it once |
| `ligus dashboard default` | prints the generated default dashboard as TOML |
| `ligus dashboard metrics` | lists the built-in Ligus metrics panels can show |
| `ligus trust list [--repo PATH]` | the runtimes and MCP servers a repository ships, and which this host trusts ([Trust this workspace](../concepts/security.md#workspace-trust)); `--repo` defaults to the current directory |
| `ligus trust enable <digest> [--repo PATH]` | trusts one, by digest prefix |
| `ligus trust enable-all [--repo PATH]` | trusts all of them |
| `ligus facts export --jsonl [--since C]` | this host's content-free facts as `ligus.facts.v1` JSON lines (measurements, guard signals, and state facts from its projection), for external observability tools; the last line carries the cursor to pass as the next `--since` (unix milliseconds also work) ([Telemetry](../concepts/security.md#facts)) |

There is no `ligus serve enroll` (a box is never a person's device): it
stops with an error that points to box admission. See
[Admitting a box](../../operate/ops/admitting-a-box.md).

```console
$ ligus serve --listen 127.0.0.1:8787 --label home-box
ligus serve: listening on http://127.0.0.1:8787 — workspace 'acme', host 63f2… ('home-box'). Ctrl-C to stop.
Run `ligus pair` to show the phone pairing code.
```

To keep `ligus serve` or `ligus worker` running, use a systemd user unit on
Linux or, optionally, a LaunchAgent on macOS that starts at login:
`deploy/local/ligus-local start --role serve|worker --data-dir DIR
--systemd|--launchd` installs either (`stop` with the same flag removes it;
`unit --launchd` prints the plist). `verify` (alias `status`) checks the unit
or LaunchAgent that `start` installed for that data dir without the flag. See
[Start at login on macOS](../../operate/deploy/personal-fleet.md#start-at-login-on-macos).

See [Remote access for your phone](../../operate/networking/remote-access.md),
[Running ligus serve as your own device](../../operate/ops/serve-box.md) and the
[serve API reference](../../operate/reference/serve-api.md).

## Workspace memory {#memory}

[Workspace memory](../features/memory.md) and live task state. Writes are
this CLI identity's and are judged like any device's: a person's identity
writes and decides; a box (a service member) can read only.

| Command | What it does |
|---|---|
| `ligus memory list [--tag a,b] [--json]` (or `ligus memory`) | the live notes, newest first |
| `ligus memory search <words…> [--tag a,b] [--json]` | notes matching every word (title, body or tags), best first |
| `ligus memory show <id> [--json]` | one note: its version (`rev`), author, source chat, tags, body and any conflicts |
| `ligus memory add <title> [--body TEXT] [--tag a,b]` | a new note; the body is `--body` or standard input |
| `ligus memory edit <id> [--title T] [--body TEXT] [--tag a,b] [--base REV]` | a new version (body from `--body` or standard input). From the current version it also settles the note's conflicts; `--base` edits from an older one (kept as a conflict) |
| `ligus memory rm <id> [--base REV]` | removes a note (its history stays in the workspace log) |
| `ligus memory proposals [--json]` | proposed changes, open first |
| `ligus memory accept <proposal-id>` / `ligus memory reject <proposal-id> [--note TEXT]` | decide a proposal |
| `ligus task-state <chat-id> [--json]` | what the agents on a chat report about their work in progress |

```console
$ echo "The team relay listens on 8443." | ligus memory add Relay ports --tag infra
0931232f-6dd8-44f6-a076-c083d9544085  rev 6924eae5-65b5-49d6-aadf-e9f2dc9a6266
$ ligus memory search relay
0931232f-6dd8-44f6-a076-c083d9544085  Relay ports [infra]  by Ana
```

## Environment variables

`ligus help env` lists every `LIGUS_*` variable the CLI reads, with its
default; a test keeps it in step with the code. The full table, with the
desktop's variables too, is in
[Environment variables](../../operate/reference/env-vars.md).

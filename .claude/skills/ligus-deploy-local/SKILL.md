---
name: ligus-deploy-local
description: Set up Ligus on this machine - the desktop app, an always-on ligus serve (the home host for the phone app) or a ligus worker - using deploy/local/ligus-local. Use when asked to install, run, check, stop or remove Ligus locally or on a home server, without Kubernetes.
---

# Set up Ligus on this machine

Read the repository's `AGENTS.md` first; its hard rules apply. Tool:
`deploy/local/ligus-local`. `ligus-local --help` prints every command, flag,
default and exit code; `ligus-local help <command>` (or `<command> --help`)
prints one command. The ligus CLI's guides cover the background:
`ligus help box`, `ligus help worker`, `ligus help agents-guide`,
`ligus help troubleshooting`, `ligus help security`, `ligus help env`. Add `--json` to read results;
exit 4 means **stop and ask the human**.

## 1. Decide what to run (checkpoint: human)

| Want | Run |
|---|---|
| Use Ligus on this Mac | the desktop app only (step 2); nothing else |
| Agents answer while the laptop sleeps; the phone app talks to this box | `ligus serve` (steps 3-7) |
| Extra capacity for agents bound to a named host | `ligus worker` (steps 3-7, `--role worker`) |

Ask which, and where the data dir goes (default suggestion `~/.ligus-serve`
or `~/.ligus-worker`). It must not be the desktop app's directory, and one
directory serves exactly one process.

## 2. Desktop app (macOS, Apple Silicon)

```sh
brew install --cask langstroth-ai/ligus/ligus-app
```

If Homebrew refuses to load the tap, the human runs `brew trust --tap
langstroth-ai/ligus`. **Stop here for the human**: first launch, passkey and
sign-in, creating or joining a workspace all happen in the app. Don't launch
the app yourself.

## 3. CLI and preflight

```sh
L=deploy/local/ligus-local
$L install-cli --json                     # latest release into ~/.local/bin; --version vX.Y.Z; --from-source builds it; --bin-dir DIR
$L preflight --role serve --data-dir ~/.ligus-serve --json
```

- `sandbox` fail: no agent sandbox here (Linux needs 6.7+ with Landlock, or
  bubblewrap; macOS has Seatbelt). **Stop**; never set
  `LIGUS_REQUIRE_SANDBOX=0`.
- `listen` fail: the port is taken; pick another `--listen`.

## 4. Configure (no secrets)

```sh
$L configure --role serve --data-dir ~/.ligus-serve [--listen 127.0.0.1:8787] [--label NAME] --json
```

`--label` defaults to `<short hostname>-<role>`; `--secret-backend` takes
`file`, `keychain` or `secret-service`; `--force` rewrites an existing env file.
Writes the env file (`~/.config/ligus-serve/ligus-serve.env`, mode 0600, no
secret values) and the data dir (0700), with `LIGUS_REQUIRE_SANDBOX=1`.
`--listen` takes a loopback, LAN or tailnet address; choosing anything but
loopback is the **human's** decision (it exposes the API to that network).
On macOS the device secrets go to the keychain, which can show a prompt the
human answers; `--secret-backend file` keeps them in owner-only files.

## 5. Enroll (checkpoint: human)

`configure` exits 4 and prints the exact command. The human runs it in their
own terminal with the invite from the desktop app (workspace > Copy invite);
the invite carries the workspace key, so never ask for it:

```sh
set -a; . ~/.config/ligus-serve/ligus-serve.env; set +a
ligus enroll -                    # paste the invite, Ctrl-D; add --token @FILE if the relay needs one
```

A box that is not a member of the workspace yet is then admitted by an
Admin: `ligus admit request` on the box prints its join request, the Admin
approves it on a desktop (workspace menu > Admit a box) and the human runs
`ligus admit redeem` with the code (`ligus help box`).

For a quick test with nothing real at stake: `configure --throwaway-workspace
--relay-url URL --force` makes a random workspace nobody else holds. It needs
a running relay: for a local one, follow the relay repository's
`relay-run-local` skill and pass its `http://127.0.0.1:<port>`.

If you chose a non-default `--env-file`, pass it to every later command
(the `next` lines repeat it).

## 6. Start and verify

```sh
$L start --role serve --data-dir ~/.ligus-serve --json        # background process, log in the data dir
$L start --role serve --data-dir ~/.ligus-serve --systemd --json   # Linux: a systemd user unit instead
$L start --role serve --data-dir ~/.ligus-serve --launchd --json   # macOS: a LaunchAgent instead (start at login)
$L unit --role serve --data-dir ~/.ligus-serve      # print the systemd unit --systemd installs (NoNewPrivileges is relaxed only for a setuid bwrap without user namespaces)
$L unit --role serve --data-dir ~/.ligus-serve --launchd   # print the plist --launchd installs
$L verify --role serve --data-dir ~/.ligus-serve --json        # process, GET /v1/health, sandbox required (alias: status; checks the systemd unit or LaunchAgent start installed)
```

If start fails, read the log it names (`ligus-local-serve.log` in the data
dir; under launchd `~/Library/Logs/ligus/serve[.<label>].log`); exit code 3
from serve means no sandbox. With `--systemd`, starting at boot needs `sudo
loginctl enable-linger $USER`: the human runs it.

`--launchd` is opt-in: ask the human before making a box start at login.
It installs `~/Library/LaunchAgents/ai.ligus.<role>[.<label>].plist` and
loads it with `launchctl bootstrap gui/<uid>`. The plist gets the env file's
settings as literal values, never secret-looking ones (`*TOKEN*`, `*KEY*`,
...), which stay in the data dir or the keychain; after editing the env
file, run `start --launchd` again. Two boxes of one role need different
`configure --label`s (a clash exits 1 and names the fix). While a box runs
under launchd, plain `stop` refuses (launchd would restart it): use
`stop --launchd`, which unloads it and deletes the plist.

## 7. Hand over (checkpoint: human)

Report the address, label and verify result. Human-only: pairing the phone
(`ligus pair` with the env loaded, in their own terminal: it prints a
secret), relay remote access (`ligus remote enable`), binding repos agents
may touch (`ligus bind-repo`), and approving the box on the desktop.
Reference: `deploy/ligus-serve/README.md`.

## Stop and remove

```sh
$L stop --role serve --data-dir ~/.ligus-serve [--launchd]   # --launchd: also no start at login
$L uninstall --role serve --data-dir ~/.ligus-serve [--delete-data]
```

`--delete-data` deletes this device's identity and only works on a data dir
ligus-local created: confirm with the human. Then remind them to revoke the
device from a desktop if it joined a real workspace.

# Install with Homebrew

On macOS, Ligus can be installed as a [Homebrew](https://brew.sh) cask from the
project's tap:

```bash
brew install --cask langstroth-ai/ligus/ligus-app
```

The desktop cask is `ligus-app` (a formula and a cask both named `ligus` would
collide in one tap).

Homebrew asks you to trust a third-party tap before it loads anything from it.
If `brew` says it is "refusing to load" a Ligus formula or cask, trust the tap
once:

```bash
brew trust --tap langstroth-ai/ligus
```

The app updates itself in place (**Settings → Updates & data**), so a plain
`brew upgrade` leaves it alone. To have Homebrew upgrade it anyway:

```bash
brew upgrade --cask --greedy ligus-app
```

This installs `Ligus.app` into `/Applications`. The desktop app is **Apple
Silicon only** — the cask is arm-only, so `brew install` on an Intel Mac reports
no available build.

The DMGs are **signed + notarized** (Developer ID), so Gatekeeper opens the app
normally — no `xattr` workaround needed — and it **updates itself in place** from
then on.

## Uninstall

```bash
brew uninstall --cask ligus-app          # remove the app
brew uninstall --zap --cask ligus-app    # also remove Ligus's local data/settings
                                         # (ai.ligus.desktop only)
```

## The `ligus` CLI / daemon

The headless client + agent daemon ships as its own formula (works on macOS
**and** Linux, Apple Silicon and x86_64):

```sh
brew install langstroth-ai/ligus/ligus   # installs `ligus`
brew upgrade ligus                  # track releases
```

This is the same runtime as the desktop app without the window — for running
agents on a server, scripting Ligus, or connecting a remote agent to a workspace.
See [Headless agents & setups](../../operate/concepts/headless-agents.md) for the full
workflow. A box (`ligus serve` or `ligus worker`) joins as its own member
through an admission an Admin approves (`ligus admit request`), never with
your invite: see [Admitting a box](../../operate/ops/admitting-a-box.md).
After `brew upgrade ligus`, restart the box so it runs the new binary
([Personal fleet](../../operate/deploy/personal-fleet.md#operating-it)), or
let `deploy/local/ligus-local upgrade --role serve --data-dir ~/.ligus-serve`
do both and check the result.

The formula also has a Homebrew service: once `deploy/local/ligus-local
configure --role serve` has written `~/.config/ligus-serve/ligus-serve.env`
and the box is admitted, `brew services start ligus` runs `ligus serve` with
that env file (at login on macOS, as a systemd user unit on Linux), and
`brew services restart ligus` restarts it after an upgrade. Without the env
file it logs why and stops (`$(brew --prefix)/var/log/ligus-serve.log`). Use
it or `ligus-local start --systemd`/`--launchd`, not both: one data dir is
one process.

## Other platforms

Homebrew installs the **CLI formula** on both macOS and Linux (above). The
**desktop app** is a macOS-only cask; on Linux it's distributed as
`.AppImage` / `.deb` / `.rpm`, and on Windows as an installer. See
[Building the dist](../../operate/ops/build.md) for all the bundle formats.

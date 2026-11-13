# Install with Homebrew

On macOS, Ligus can be installed as a [Homebrew](https://brew.sh) cask from the
project's tap:

```bash
brew install --cask ligus-ai/ligus/ligus-app
```

The desktop cask is `ligus-app` (a formula and a cask both named `ligus` would
collide in one tap). If you installed the former `hive` cask or `hive-cli`
formula from the old tap, `brew upgrade` moves you to `ligus-app` and `ligus`.

Upgrade later with:

```bash
brew upgrade --cask ligus-app
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
brew uninstall --zap --cask ligus-app    # also remove local data/settings (both the
                                         # ai.ligus.desktop and former com.hive.desktop dirs)
```

## The `ligus` CLI / daemon

The headless client + agent daemon ships as its own formula (works on macOS
**and** Linux, Apple Silicon and x86_64):

```sh
brew install ligus-ai/ligus/ligus   # installs `ligus`, plus a `hive` alias through v2.1
brew upgrade ligus                  # track releases
```

This is the same runtime as the desktop app without the window — for running
agents on a server, scripting Ligus, or connecting a remote agent to a workspace.
See [Headless agents & setups](../concepts/headless-agents.md) for the full
workflow (`ligus enroll` → `ligus worker`).

## Other platforms

Homebrew installs the **CLI formula** on both macOS and Linux (above). The
**desktop app** is a macOS-only cask; on Linux it's distributed as
`.AppImage` / `.deb` / `.rpm`, and on Windows as an installer. See
[Building the dist](../ops/build.md) for all the bundle formats.

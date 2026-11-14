# Upgrading from Hive 1.x

Hive became **Ligus** in 2.0. *Ligus* is short for *ligustica*
(*Apis mellifera ligustica*, the Italian honeybee). The wire protocol and
your data are unchanged: workspaces, device identities, relays and paired
phones keep working across the upgrade. What changes is names: the app, the
CLI, environment variables, directories and the config file.

This page is the checklist for every component, in the order to do it, with
the rollback for each.

## What moves, and how

| Thing | Hive 1.x | Ligus 2.0 | Migrated how | Old copy kept? |
|---|---|---|---|---|
| Desktop data dir | `com.hive.desktop` | `ai.ligus.desktop` | **copied** on first launch, before the database opens | **Yes**, untouched |
| CLI data dir (no `LIGUS_DATA_DIR` set) | `~/.hive` | `~/.ligus` | **renamed** on first CLI run | No (renamed) |
| Workspace state dir in each repo | `<repo>/.hive/` | `<repo>/.ligus/` | **renamed** (or merged) the first time the workspace is used; per-chat worktrees re-registered with `git worktree repair` | No (renamed) |
| Config file | `hive.config.toml` | `ligus.config.toml` | **renamed** on first read, when only the old one exists | No (renamed) |
| Environment variables | `HIVE_*` | `LIGUS_*` | **not migrated**: rename them yourself | n/a |
| CLI binary | `hive` | `ligus` | Homebrew upgrade | n/a |
| Homebrew | formula `hive-cli`, cask `hive`, tap `honeyhive-ai/hive` | formula `ligus`, cask `ligus-app`, tap `ligus-ai/ligus` | `brew upgrade` | n/a |
| Phone deep links | `hive://pair?…` | `hive://` still emitted; `ligus://` also accepted | nothing to do | n/a |

Details, from the code:

- **Desktop copy.** The app copies the old data dir into a temporary sibling,
  fsyncs every file, then renames it into place, so a crash leaves either no
  new dir or a complete one. It writes a `MIGRATED_FROM` file holding the old
  path. If the copy fails (disk full, permissions) the app shows *Could not
  move your data* and refuses to start rather than open an empty database and
  mint a new device identity. The old dir is never modified or deleted
  (`app/src/data_migration.rs`). If `ai.ligus.desktop` already holds data, the
  copy is skipped.
- **CLI rename.** `~/.hive` is renamed to `~/.ligus` only when `~/.ligus`
  doesn't exist and `~/.hive` holds CLI data (`hive.db` or `identity.json`).
  A failed rename stops the CLI with an error rather than starting fresh
  (`crates/hive-core/src/compat.rs`, `home_data_dir`). If you set
  `LIGUS_DATA_DIR`, nothing is moved.
- **Workspace dir.** With no `.ligus/` yet, `.hive/` is renamed. If both
  exist, entries `.ligus/` lacks are moved and clashing ones stay in `.hive/`.
  Worktrees git can't re-register leave `.hive` as a symlink to `.ligus`
  (Unix) so git's recorded paths keep resolving. `$HOME/.hive` is never
  treated as workspace state.
- **Environment variables.** Only `LIGUS_*` names are read. A `HIVE_X` that is
  set while `LIGUS_X` is not is **ignored**, and Ligus prints once:
  `warning: HIVE_X is set but no longer read; rename it to LIGUS_X`.
- The file names inside data dirs (`hive.db`, `serve.token`, ...) and the
  protocol identifiers that contain `hive` (`hivews1:` invites, `hrt1`
  tokens, `hive-tunnel/1`, the `@hive` agent) keep their names. See
  [Wire protocols](../reference/wire-protocols.md#legacy-protocol-identifiers).

## Before you start

1. **Back up.** Copy each data dir somewhere safe while nothing is running.
   See [Backups and restore](../ops/operations.md#backups-and-restore).

    ```sh
    # macOS desktop
    cp -a ~/Library/Application\ Support/com.hive.desktop ~/hive-desktop-backup
    # CLI / ligus serve / worker box
    cp -a ~/.hive ~/hive-cli-backup            # or your HIVE_DATA_DIR
    ```

2. **List your `HIVE_*` variables** everywhere they are set: shell profile,
   systemd env files, container specs, CI.

    ```sh
    env | grep '^HIVE_'
    grep -rn 'HIVE_' ~/.zshrc ~/.bashrc ~/.profile ~/.config/hive-serve/ 2>/dev/null
    ```

3. **Upgrade the relay last, or not at all.** A 2.0 client talks to a 1.x
   relay, and the relay keeps no client data that needs migrating. See
   [Upgrading the relay](../networking/self-host.md#upgrades).

## Step 1: the desktop app

```sh
brew update
brew upgrade --cask ligus-app      # replaces the hive cask
```

A self-updating install updates itself; it may stay at
`/Applications/Hive.app`, which you can rename to `Ligus.app`.

On first launch, the app copies your data. Expected result: your workspaces,
chats and identity are all there, with no onboarding. Theme and pane layout
reset once, and macOS asks again for folder and notification access.

Check:

```sh
cat ~/Library/Application\ Support/ai.ligus.desktop/MIGRATED_FROM
# /Users/you/Library/Application Support/com.hive.desktop
```

The first time you open each workspace, its `.hive/` becomes `.ligus/`:

```sh
ls -a ~/code/myrepo | grep -E '^\.(hive|ligus)$'
# .ligus
```

## Step 2: CLI boxes (`ligus serve`, `ligus worker`)

```sh
brew update && brew upgrade        # hive-cli -> ligus
ligus --version
# ligus 2.0.0
```

Rename every variable in the service's env file and unit. With the shipped
systemd user unit:

```sh
systemctl --user stop hive-serve
mkdir -p ~/.config/ligus-serve
sed 's/^HIVE_/LIGUS_/' ~/.config/hive-serve/hive-serve.env > ~/.config/ligus-serve/ligus-serve.env
chmod 600 ~/.config/ligus-serve/ligus-serve.env
# install the new unit (deploy/ligus-serve/ligus-serve.service) and point it at the new binary
systemctl --user disable hive-serve
systemctl --user daemon-reload
systemctl --user enable --now ligus-serve
journalctl --user -u ligus-serve -n 50
```

Check the log for `warning: HIVE_… is set but no longer read`. Any such line
names a variable you missed.

If the box used the default data dir, the first run prints:

```text
note: moved /home/you/.hive to /home/you/.ligus
```

If your env file sets `HIVE_DATA_DIR=/srv/hive`, renaming it to
`LIGUS_DATA_DIR=/srv/hive` is all you need; the directory is used where it is.

Then check the box is healthy:

```sh
ligus whoami                 # same account and device ids as before
ligus remote status          # same host id; phones stay paired
curl -s http://<listen>/v1/health -H "Authorization: Bearer $(cat $LIGUS_DATA_DIR/serve.token)"
```

## Step 3: phones

Nothing to do. Pairings, credentials and push keys live in the box's data dir,
which moved with it, and the `hive://` pairing links keep working.

## Step 4: the relay (optional)

See [Self-hosting a relay: upgrades](../networking/self-host.md#upgrades).
Relay v0.2.0 reads both `LIGUS_RELAY_*` and the former `HIVE_RELAY_*` names
(the `LIGUS_` name wins and a legacy name logs a deprecation warning), so
you can rename its variables at your own pace. Unlike the relay, the `ligus`
CLI and the desktop app do **not** fall back to `HIVE_*` names.

## Rollback

You can go back to Hive 1.x. The protocol is the same, so a 1.x client keeps
syncing with 2.0 peers.

| Component | To roll back |
|---|---|
| Desktop | Reinstall the 1.x app. Its data dir `com.hive.desktop` was never touched, so it opens as it was **before** the upgrade. Events created after the upgrade come back through sync from the relay or peers; anything that only existed on this device after the upgrade (local settings, unsynced drafts) stays in `ai.ligus.desktop`. |
| CLI box | Stop the service, reinstall `hive-cli`, and either set `HIVE_DATA_DIR=$HOME/.ligus` or `mv ~/.ligus ~/.hive`. Rename the env file's variables back to `HIVE_*`. |
| Workspace dir | `mv .ligus .hive` in each repo, then `git worktree repair` there. If `.hive` is a symlink to `.ligus`, remove the symlink first. |
| Config file | `mv ligus.config.toml hive.config.toml`. |

!!! warning "Don't run both versions on one data dir"
    Each data dir is one device. Never point a 1.x and a 2.0 process at the
    same directory at the same time: two writers on one `hive.db` and one
    device key is the one thing the sync model can't recover from cleanly.

## Troubleshooting the upgrade

**The desktop shows *Could not move your data*.** The copy failed; nothing was
changed. Free disk space (you need room for a second copy of the data dir) or
fix permissions, then reopen the app.

**The desktop opened empty / runs onboarding.** `ai.ligus.desktop` already
held data from an earlier 2.0 trial, so the copy was skipped. Quit the app,
move `ai.ligus.desktop` aside, and reopen it to copy again.

**`error: could not move ~/.hive to ~/.ligus`.** Move it by hand or set
`LIGUS_DATA_DIR` to the old path.

**A setting seems to have no effect.** It is probably still spelled `HIVE_*`.
Look for the one-time `warning: … no longer read` line in the output.

**`git worktree` errors in a chat checkout.** Run `git worktree repair` in the
repository; the migration does this, but a worktree on another volume can
need it again.

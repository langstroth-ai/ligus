# Upgrading from Hive 1.x

**Ligus 0.1.0** is a new application, the successor to Hive. *Ligus* is
short for *ligustica* (*Apis mellifera ligustica*, the Italian honeybee).

**Ligus 0.1 starts fresh.** Nothing from Hive 1.x is migrated: no chats,
workspaces, device identity, keys, settings or pairings. You create new
workspaces, invite your teammates again and re-pair your phones.

In exchange, 0.1 never touches your 1.x data. It does not read, modify, move,
rename, copy or delete any Hive 1.x file or directory. Your 1.x install keeps
working exactly as before, side by side with 0.1, until you remove it.

## Where each version keeps its data

| Thing | Hive 1.x (left as it is) | Ligus 0.1 (starts empty) |
|---|---|---|
| Desktop data dir (macOS) | `~/Library/Application Support/com.hive.desktop` | `~/Library/Application Support/ai.ligus.desktop` |
| Desktop data dir (Linux) | `~/.local/share/com.hive.desktop` | `~/.local/share/ai.ligus.desktop` |
| Desktop data dir (Windows) | `%APPDATA%\com.hive.desktop` | `%APPDATA%\ai.ligus.desktop` |
| CLI data dir (no data-dir variable set) | `~/.hive` | `~/.ligus` |
| Workspace state dir in each repo | `<repo>/.hive/` | `<repo>/.ligus/` |
| Config file | `hive.config.toml` | `ligus.config.toml` |
| Environment variables | `HIVE_*` | `LIGUS_*` |
| CLI binary | `hive` | `ligus` |
| Homebrew | formula `hive-cli`, cask `hive` | formula `ligus`, cask `ligus-app` |

What 0.1 does with the 1.x names:

- **Data dirs.** 0.1 uses only its own. It refuses to start on a data dir that
  is a 1.x location, sits inside one, contains one, or is a symlink to one,
  and on any dir that already holds `hive.db` or `identity.json` that 0.1 did
  not create (for example a 1.x serve box's `~/.hive-serve`). The error names
  the dir, and nothing is written. Point `LIGUS_DATA_DIR` at a new, empty dir.
  0.1 marks the dirs it owns with a `.ligus-data-dir` file.
- **`~/.hive`.** Left as it is. On the first CLI run with no `~/.ligus` yet,
  `ligus` prints once:
  `note: Ligus starts fresh in /home/you/.ligus; the Hive 1.x data in /home/you/.hive is left as it is`.
- **A repo's `.hive/`.** Ignored and never changed. 0.1 keeps its state in
  `.ligus/` next to it, with its own chat checkouts under `.ligus/worktrees/`.
  The worktree cleanup only ever looks there, so 1.x checkouts under
  `.hive/worktrees/` and their `hive/chat/*` and `hive/turn-*` branches are
  never removed. A `.ligus` that is a symlink into `.hive` is refused.
- **`hive.config.toml`.** Not read and not renamed: it may be committed and
  still used by teammates on 1.x, and reading it would carry 1.x relay and
  runtime settings into a fresh install. When a dir has one and no
  `ligus.config.toml`, Ligus prints once:
  `note: <path>/hive.config.toml is a Hive 1.x config and is not read; Ligus reads ligus.config.toml (copy the settings you want into it)`.
  The format is the same, so `cp hive.config.toml ligus.config.toml` is a
  fine start if you want the same runtimes and MCP servers.
- **Environment variables.** Only `LIGUS_*` names are read. A `HIVE_X` that is
  set while `LIGUS_X` is not is **ignored**, and Ligus prints once:
  `warning: HIVE_X is set but no longer read; rename it to LIGUS_X`.
- **Agents.** The 1.x data dirs stay on the sandbox's deny-read list, because
  they still hold your 1.x keys: no agent that 0.1 starts can read them.
- Protocol identifiers that contain `hive` (`hivews1:` invites, `hrt1` relay
  tokens, `hive-tunnel/1`, `hive://` pairing links, `hive/chat/<id>` branch
  names) keep their names. See
  [Wire protocols](../reference/wire-protocols.md#legacy-protocol-identifiers).

## Setting up 0.1

### The desktop app

```sh
brew update
brew install --cask ligus-app
```

`Ligus.app` installs next to `Hive.app`, and both can run. Ligus opens with
onboarding and an empty workspace list: create or join your workspaces again.

### CLI boxes (`ligus serve`, `ligus worker`)

Install `ligus` (the `hive` binary is not replaced) and set the box up as a
new one, with its own data dir, env file and unit. The 1.x service keeps
running until you stop it.

```sh
brew update && brew install ligus
mkdir -p ~/.config/ligus-serve
cp deploy/ligus-serve/ligus-serve.env.example ~/.config/ligus-serve/ligus-serve.env
chmod 600 ~/.config/ligus-serve/ligus-serve.env
# edit it: LIGUS_* names only, a NEW data dir (e.g. LIGUS_DATA_DIR=$HOME/.ligus-serve)
# and, while the 1.x service still runs, a different listen port
systemctl --user daemon-reload
systemctl --user enable --now ligus-serve
```

Don't reuse the 1.x data dir as `LIGUS_DATA_DIR`: 0.1 refuses it. See
[`deploy/ligus-serve/README.md`](https://github.com/ligus-ai/ligus/tree/main/deploy/ligus-serve)
for the full setup.

### Phones

Pair again with `ligus pair` on the 0.1 box. A 1.x pairing keeps pointing at
the 1.x box.

### The relay

Nothing to do. The relay keeps no client data, and 0.1 clients use the same
relay API (`/v1` and `hrt1` tokens are unchanged); new 0.1 workspaces get new
rooms. See [Self-hosting a relay: upgrades](../networking/self-host.md#upgrades).
Relay v0.2.0 reads both `LIGUS_RELAY_*` and the former `HIVE_RELAY_*` names
(the `LIGUS_` name wins and a legacy name logs a deprecation warning). Unlike
the relay, the `ligus` CLI and the desktop app do **not** fall back to
`HIVE_*` names.

## Removing Hive 1.x when you're done

Only when you no longer need anything in it; 0.1 never needs these files.
Stop every 1.x process first, and back up anything you want to keep.

```sh
# Desktop (macOS): quit Hive, then
brew uninstall --cask hive            # or drag Hive.app to the Trash
rm -rf ~/Library/Application\ Support/com.hive.desktop \
       ~/Library/Caches/com.hive.desktop \
       ~/Library/Preferences/com.hive.desktop.plist \
       ~/Library/Saved\ Application\ State/com.hive.desktop.savedState
# Desktop (Linux): ~/.local/share/com.hive.desktop and ~/.config/com.hive.desktop

# CLI / serve / worker boxes
systemctl --user disable --now hive-serve 2>/dev/null
brew uninstall hive-cli               # or remove the hive binary
rm -rf ~/.hive                        # and any HIVE_DATA_DIR, e.g. ~/.hive-serve
rm -rf ~/.config/hive-serve

# Each repo: 1.x chat checkouts, state and config
cd ~/code/myrepo
git worktree list | grep '/\.hive/worktrees/'      # review first
git worktree list --porcelain | sed -n 's|^worktree \(.*/\.hive/worktrees/.*\)|\1|p' |
  while read -r wt; do git worktree remove --force "$wt"; done
git worktree prune
rm -rf .hive hive.config.toml        # only once no teammate still uses 1.x
git branch --list 'hive/chat/*' 'hive/turn-*'      # delete the ones you don't need
```

`brew uninstall --zap --cask ligus-app` removes only Ligus's own
`ai.ligus.desktop` files. It never removes Hive 1.x data.

## Troubleshooting the upgrade

**`... overlaps the Hive 1.x data at ...` or `... holds data Ligus did not
create`.** The data dir in use is, or links to, 1.x data. Nothing was
changed. Unset `LIGUS_DATA_DIR` or point it at a new, empty dir. On the
desktop, check that `ai.ligus.desktop` is not a symlink.

**`.ligus ... resolves into Hive 1.x state`.** The repo's `.ligus` is a
symlink into `.hive`. Remove the link and open the workspace again.

**A setting seems to have no effect.** It is probably still spelled `HIVE_*`,
or it lives in `hive.config.toml`. Look for the one-time `warning: … no
longer read` or `note: … is not read` line in the output.

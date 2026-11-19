# Operations

Running Ligus day to day: health checks, logs, backups and restores, and
upgrades. For problems by symptom, see [Troubleshooting](../../ligus/help/troubleshooting.md).

## What to monitor

| Component | Check | Healthy |
|---|---|---|
| Relay | `curl -fsS https://relay.example.com/v1/health` | `ok` (200). `503` = the relay runs but its store doesn't answer |
| Relay | `GET /metrics` (bearer `LIGUS_RELAY_METRICS_TOKEN` if set) | `hive_relay_requests_total`, `hive_relay_requests_by_class`, `hive_relay_requests_in_flight` |
| Relay tunnel | `curl -s https://relay.example.com/v1/tunnel/info` | `{"versions":[1],…}`; 404 = tunnel off |
| `ligus serve` | `curl -s http://<listen>/v1/health` | `{"ok":true,"version":"0.1.0"}`; with the bearer token also `identity.mode` (`owner`, `member`; `pending` or `revoked` means it can't write) |
| `ligus serve` relay link | `ligus remote status` (server's env) | `serve: connected for …` |
| `ligus serve` sync | the dashboard's built-in Ligus section (relay and push status, turns) | |
| `ligus worker` | the process is running; its host shows **online** in `ligus queue` / the desktop's Review pane | there is no health endpoint |
| Desktop | the sync pill | **live**, encrypted |

A host counts as online while it beats on the relay's presence channel
(about every 30 s; offline after about 90 s without a beat). `ligus queue`
shows unanswered mentions and whether each agent's host is online.

For a home box, a cron job is enough:

```sh
*/5 * * * * curl -fsS -m 10 http://100.101.102.103:8787/v1/health >/dev/null || echo "ligus serve down" | mail -s ligus you@example.com
```

## Logs

| Component | Where | Level |
|---|---|---|
| Desktop | daily file `ligus.log.<date>` in the OS log dir for `ai.ligus.desktop` (macOS `~/Library/Logs/ai.ligus.desktop/`), also stderr. **Settings → Diagnostics** links to it | `LIGUS_LOG` (default `info`; `tracing` filter syntax) |
| `ligus serve`, `ligus worker` | stdout/stderr; with the shipped unit, `journalctl --user -u ligus-serve` | fixed |
| `ligus serve` push decisions | stdout, `push:` lines (one per notification and phone: sent, skipped and why, relay's answer) | fixed |
| Relay push results | stderr, `tunnel push` lines (`result`, and for APNs `env`, `topic`, `status`, `reason`, `apns_id`; never the device token) | info |
| Relay | stdout/stderr | fixed |

Ligus logs ids, kinds and reasons, not message content. Each device also
keeps a local **guard log** (the `guard_signals` table in `hive.db`, newest
20,000 entries, never synced): ingest rejections, tool role-gate decisions,
MCP tools offered and dropped, redactions, lockdowns, worktree clean-ups,
throttles. Read it with any SQLite client:

```sh
sqlite3 "$LIGUS_DATA_DIR/hive.db" "SELECT * FROM guard_signals ORDER BY rowid DESC LIMIT 20;"
```

## Backups and restore

Every device keeps its own full copy of the workspaces it belongs to, so a
workspace survives as long as one device does. Back up each device's **data
dir** anyway: it holds that device's identity, which can't be recreated, and
any events it hadn't synced yet.

### What's in a data dir

| File | Desktop | CLI (`serve`/`worker`) | Contains |
|---|---|---|---|
| `hive.db` (+ `-wal`, `-shm`) | yes | yes | all events (SQLite, WAL mode) |
| `identity.json` | yes | yes | public identity |
| `secrets/` (`ligus.keychain-db` on macOS), `credentials/` (systemd-creds), or `keys/` (file fallback) | yes | yes | **private key seeds**, workspace keys, API keys and tokens (see [Key storage](../../ligus/concepts/identity.md#key-storage)) |
| `settings.json` | yes | | settings; secrets only as `{"set": true}` |
| `config.toml` | | yes | relay URL and room (the workspace key and relay token are in the secret store) |
| `serve.token`, `serve-state.json` | | serve | direct-path token, read marks |
| `tunnel/` | | serve | host keys, paired phones, relay settings |
| `repo-bindings.toml`, `repos/` | | serve/worker | bound checkouts |
| `dashboard.toml` | | serve | dashboard config |
| `attachments/` | yes | yes | composer files, verified copies of shared files (`verified/`, private), agents' offered files waiting for approval (`staged/`) |

Backups contain keys: encrypt them and keep them private.

### Take a backup

The safest backup is of a stopped process. For a running one, copy the
database with SQLite's online backup, then the rest:

```sh
D=$HOME/.ligus-serve                      # or the desktop's app data dir
B=/backups/ligus-$(date +%F)
install -d -m 700 "$B"
sqlite3 "$D/hive.db" ".backup '$B/hive.db'"
rsync -a --exclude 'hive.db*' --exclude 'repos/' "$D/" "$B/"
tar -C "$B" -czf "$B.tgz" . && rm -rf "$B"
```

Don't copy `hive.db` alone with `cp` while the process runs: the WAL file
may hold the newest events.

Also back up your repositories the usual way (push them). Worktrees under
`<repo>/.ligus/worktrees` are disposable; unapplied agent work is in the
proposals.

### Restore

1. Stop the process (or quit the app).
2. Move the current data dir aside; unpack the backup in its place with mode
   `0700`.
3. Start it. It pushes anything it has that the relay lacks and pulls what
   happened since the backup.

!!! danger "Restore onto one machine only"
    A data dir is one device. Restoring the same backup onto two machines
    gives two writers the same device key, which the sync model can't
    reconcile cleanly. To move a device, stop the old one first. To add a
    machine, enroll a new device instead.

### Lost a device without a backup

Revoke it (your own device: **Settings → Account → My devices**) and set up
a new one: a desktop by signing in and joining the workspace again; a serve
or worker box with `ligus enroll` in a fresh data dir. History comes back
from the relay or peers, as far back as the relay's retention reaches.

### The relay

See [Self-hosting a relay: backups](../networking/self-host.md#backups).
A relay backup is a convenience, not your only copy.

## Upgrades

| Component | Upgrade | Check |
|---|---|---|
| Desktop | updates itself (or `brew upgrade --cask ligus-app`) | **Settings → Updates & data** shows the version |
| CLI | `brew upgrade ligus`, then restart services | `ligus --version` |
| Relay | replace binary/image, restart | `/v1/health` → `ok` |

Devices on different 2.x versions sync with each other; the wire protocol is
kept compatible.

## Housekeeping Ligus does itself

- **Hygiene sweep**, 3 minutes after launch and every 6 hours: removes
  leftover turn worktrees (saving unfinished edits under
  `.ligus/stopped-turns/`), checkouts of deleted or archived chats (their
  `hive/chat/…` branches are kept), and marks proposals whose diff no longer
  applies as stale. **Settings → Guards → Sweep now** runs it at once. See
  [Hygiene](../../ligus/features/guards.md#hygiene).
- **Foreign-workspace purge**: a worker removes events of other workspaces
  that older clients pushed into its room, after a `hive.db.bak-foreign-purge-<time>`
  backup next to the database.
- **Plan-window throttles** pause scheduled runs, then workflow stages and
  hand-offs, near a subscription's usage limit. See
  [Plan windows](../../ligus/features/guards.md#plan-windows-staggered-throttles).

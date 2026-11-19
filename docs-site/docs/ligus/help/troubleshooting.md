# Troubleshooting

Problems by symptom. Each section starts with the quickest check. For
health endpoints and logs, see [Operations](../../operate/ops/operations.md).

## Which version am I running?

- **Desktop app**: **Settings → Updates & data** shows the version.
- **`ligus` CLI** (on a `ligus serve` or `ligus worker` host):
  ```bash
  ligus --version     # prints: ligus <version>   (-V and `ligus version` work too)
  ```
- **`ligus serve` from the phone or curl**: `GET /v1/health` returns
  `{"ok":true,"version":"…"}`.

Include all of them when you report a problem.

## App won't launch

Release DMGs for macOS are signed and notarized. Windows and Linux release
builds, and builds from source, are unsigned, so the OS blocks the first
launch:

- **macOS** (a build from source): right-click → **Open**, or
  `xattr -dr com.apple.quarantine /Applications/Ligus.app`.
- **Windows**: SmartScreen → **More info → Run anyway**.
- **Linux**: `chmod +x Ligus_*.AppImage` (add `--appimage-extract-and-run`
  on hosts without FUSE).

**"Ligus can't use this data folder"** on launch: the data dir is, or links
to, data Ligus did not create. Nothing was changed. Point `LIGUS_DATA_DIR`
at a new, empty dir.

## Sync is stuck or peers don't see each other

**Start with the sync pill.** It shows **live**, **error** (with the
reason), **offline**, or an amber **limited** state (see
[The relay refuses writes](#relay-refusals)). Then, on every device:

1. **Same relay URL and room** (Settings → Team sync). Workspaces created
   with **Add workspace → Create** have random room ids; join them with the
   invite, don't retype them.
2. **The relay is up**: `curl https://<relay>/v1/health` → `ok`. `503` means
   the relay runs but its store doesn't answer.
3. **A workspace key is set and identical everywhere.** Sync refuses to run
   without one: `sync paused: this workspace is configured for a relay but
   has no end-to-end-encryption key`. A wrong key shows as a sync error
   rather than silently dropping events.
4. **The relay accepts the device's token**: see [Relay 401s](#relay-401s).
5. **Only one process per data dir.** Two `ligus` processes on one data dir
   fight over one device identity.
6. **A device offline for a long time** catches up from the relay only as
   far back as the relay retains (by default the newest 50,000 envelopes per
   workspace); older events come from a peer that still has them.

Notes:

- A single undecryptable or corrupt envelope doesn't stop sync; it is
  skipped and logged.
- An event this device can't judge yet (signed by a device whose
  certificate for that person hasn't arrived, by someone not admitted yet,
  or for an account whose key is disputed) doesn't stop sync either: it
  waits aside and applies on its own once what it waits for arrives.
  Settings → Team sync says how many wait ("N events wait for a device
  certificate or an admission"); on a box, `ligus doctor` prints `held
  events: N from M device(s), D dropped by a cap (R to fetch again from the
  relay), L given up, E expired` and the reasons. A few that never go away
  are normal when someone wrote from a device nobody certified (or as
  someone they aren't). A device keeps at most 1,000 per signing device,
  10,000 per workspace, and none past 30 days. One dropped by a cap is
  fetched again from the relay once what it waited for arrives; only what
  is given up (past every limit) is missing on that device, and it is
  counted there.
- **A person's events stopped appearing everywhere** and they recently
  re-installed or signed in on a new device: check that the new device was
  added to their account (Settings → Passkeys & keys, **Confirm this
  device**). Someone else registering a key under their account can't do
  this: such a registration is refused and named in the device's guard log
  (`account_key_refused`), and their events keep folding. Only their own
  devices asserting two different account keys stops their events (held,
  never applied); remove the device that registered the second key and
  re-invite them under a new account.
- A headless box (`ligus serve`, `ligus worker`) stops seeing new messages
  from desktops after an Admin used **Remove & revoke**: it can't follow a
  key rotation. See [Offboarding](../../operate/security/runbooks.md#offboarding).
- Relays with a snapshot or Postgres store keep queued events across
  restarts; a relay with neither (pure memory) loses them.
- On a CLI box, `ligus sync` prints `pushed N, pulled M`; errors print
  `sync: <reason> — retrying in Ns` in the worker's log (backing off up to
  120 s).

## The relay refuses writes (workspace full, quotas, rate limits) {#relay-refusals}

A relay answers a limit with `429` or `507`, a code and `Retry-After`. Ligus
acts on the code and never loops against it:

| Pill / message | Codes | What Ligus does | What you do |
|---|---|---|---|
| **Workspace full** | `507` `room_bytes`, `room_envelopes`, `keyring` | stops writing to that workspace only; it stays readable and other workspaces keep syncing. New events wait on this device and one push is tried again after `Retry-After` (an hour) | ask the relay operator for more room, or move the workspace to another relay |
| **Organisation quota reached** | `507` `tenant_bytes`, `tenant_rooms` | as above | ask your organisation's admin |
| **Relay full** | `507` `store_bytes`, `total_bytes`, `total_rooms`, `contacts_full` | backs off until `Retry-After` (ten minutes), then retries | nothing, unless it lasts: tell the relay operator |
| **Sync slowed by the relay** | `429` `ip_rate`, `room_rate`, `write_rate`, `read_rate`, `room_writer_rate`, `tenant_rate`, `contact_*_rate`, `rate_limited` | waits `Retry-After` and sends the same request again (same bytes, same idempotency key, so nothing is stored twice) | nothing |
| (no pill) | `429` `sse_streams` | closes back to polling until `Retry-After` | close other Ligus windows or devices using the same token |
| "too many wrong pairing codes" | `429` `pair_lockout` | sends no pairing-code lookup to that relay until it ends | wait, then type the code again |
| "Not retried right away" | `429` `client_rooms`, `too_many_pending` | pauses this workspace's writes; tries one push again after an hour (or the relay's longer `Retry-After`) | delete a workspace you no longer use (or clear pending requests); restart Ligus (or `ligus serve`) to retry at once |
| **Room expired or truncated** | a new device's first envelope isn't the workspace's first | syncs what is there and says so: the device can't rebuild the workspace from its founding | ask a member whose device still holds it to re-invite you, or move the workspace to a relay that keeps the whole log |

An unknown code is handled by its status: an unknown `429` like a rate
limit, an unknown `507` like a full relay. On a CLI box the same messages
appear as `sync: <message>` in the log and in the dashboard's sync state.

## Relay 401s

| Message | Cause | Fix |
|---|---|---|
| desktop: `sync paused: the relay rejected this device's access token` | the relay is gated and this device has no token, or a wrong, expired or revoked one | paste a valid token in **Settings → Team sync → Relay access token** |
| CLI: an unauthorized (401) error from the relay | same, for `ligus` | `ligus enroll - --token @<token-file>` (paste the invite) or `LIGUS_RELAY_ACCESS_TOKEN` |
| `ligus enroll`: "this invite can't be used: its workspace has no relay", or `ligus serve`/`sync`: "this box's workspace has no relay URL" | the invite came from a workspace with no relay set | in the desktop, set **Settings → Team sync → Relay URL**, copy a new invite, and run `ligus enroll` with it again |
| desktop, pasting an invite: "you've already joined … and this invite doesn't match it" or "… on another relay" | an invite for a workspace this device already has, with a different key, founder or relay. An invite never replaces those: anyone who knows the room could forge one | if the workspace really moved relays, change it under **Settings → Team sync → Relay URL**; otherwise ignore the invite and ask its sender how they got it |
| People says "your relay doesn't support contacts yet" | the relay is older than v0.3 (or contacts are turned off) | nothing breaks: send invite links instead, or upgrade the relay |
| writes or reads refused on a membership relay | the workspace is claimed and you're not on its relay roster | an Admin adds you, or redeem an invite code with `ligus join` |

Check a token directly:

```sh
curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer $TOKEN" \
  https://relay.example.com/v1/workspaces/test/envelopes
# 200 = accepted, 401 = refused
```

Signed (`hrt1`) tokens expire if issued with `--exp-days`. A token given as
`name:token` works; the `name:` part is dropped.

**`ligus serve`'s own API** answers `401 {"error":"missing or invalid bearer
token"}` when the phone's direct-path token is wrong, usually because
`serve.token` was rotated (`ligus phones revoke` does this). Pair again.

## The phone can't connect

Run these on the box, with the server's environment
(`set -a; . ~/.config/ligus-serve/ligus-serve.env; set +a`):

```sh
ligus remote status
curl -s http://<listen>/v1/health
```

| Symptom | Likely cause | Fix |
|---|---|---|
| `ligus serve` won't start: bind refused | `LIGUS_SERVE_LISTEN` is `0.0.0.0` or a public address | use a loopback, LAN or Tailscale address |
| `ligus pair` says the link would point the phone at itself | it read the default loopback address | load the server's env, or `--listen` / `--public-url`, or turn on the relay tunnel |
| `ligus pair` says there's no serve token | wrong data dir, or serve never ran | set `LIGUS_DATA_DIR` / `--token-file`, or start `ligus serve` |
| phone: link expired or already used | pairing secrets are one-time, 10 minutes, and need serve running | `ligus pair` again |
| phone: "add this phone first" (close `4040 device_not_certified`) | the box doesn't know the phone's device key as yours yet | on the desktop, **My devices > Add this phone** with the app's code; let the box sync; `ligus pair` again |
| phone: device revoked (close `4041 device_revoked`) | that phone was revoked | reset the phone's device key in the app and add it again |
| direct path times out | phone not on the same LAN or tailnet; firewall | check Tailscale on both; `curl` the URL from another device |
| `serve: not connected` | relay tunnel off (`/v1/tunnel/info` 404), wrong or `http` URL, missing `--reg-token`, two serve processes on one data dir | see the `last error` |
| `serve: unknown … is it running?` | `ligus serve` stopped | `systemctl --user status ligus-serve` |
| `status pending` | relay registered the box but hasn't activated it (allowlist) | add the host id to `LANGSTROTH_RELAY_TUNNEL_HOSTS` |
| phone: host offline | the box has no live relay connection | `ligus remote status` |
| phone: revoked or credential expired | revoked, or unused for 90 days | pair again |
| writes fail with 403 | the box's role doesn't allow it, or it is `pending`/`revoked` as your device | `/v1/health` with the token shows `identity.mode` |

More: [Remote access](../../operate/networking/remote-access.md#troubleshooting).

## No push notifications

Run `ligus remote push-test` on the box. It sends a test notification to each
paired phone and prints every hop: whether the phone asked this box for
notifications, whether the box handed the push to the relay, and what APNs
answered (environment, topic, status and reason), with the fix for the usual
failures. Then read the `push:` lines in the `ligus serve` log for real
notifications (each one says sent, skipped because the app is open,
rate-limited, or refused by the relay and why). The full checklist is in
[Troubleshooting push](../features/phone-app.md#troubleshooting-push).

## A worker (or serve box) doesn't pick up turns

1. **Is the agent bound to this host?** `ligus agents` and `ligus hosts`
   (or the agent's host badge in the desktop). An agent with no host is
   answered by its owner's device; `ligus set-agent-host <agent> <host-id>`
   moves it.
2. **Is the worker online?** `ligus queue` shows each queued mention and
   whether its agent's host is online. The worker beats every 30 s; it
   counts as offline after 90 s.
3. **Was the agent mentioned?** Workers answer `@agent` mentions for their
   agents. `@ligus` (the chat's primary runtime) is answered only by a
   desktop. On a serve box, `LIGUS_SERVE_DEFAULT_AGENT` routes un-mentioned
   phone messages to an agent.
4. **Does the runtime resolve?** A worker uses workspace runtimes only
   (never a personal key). A runtime with a `secretRef` needs
   `LIGUS_WS_SECRET_<ref>` in the worker's environment; a subscription CLI
   needs to be signed in on that box (`ligus setup` shows which).
5. **Did a turn hard-fail?** A mention whose reply failed isn't retried every
   tick; restart the worker to retry, and check its log for the error.
6. **No tools, or no edits?** Tools run only for authors at or above
   `LIGUS_MCP_MIN_ROLE` (default Admin on a write-capable tier, Contributor
   when read-only). Claude Code on a worker is read-only unless
   `LIGUS_WORKER_PERMISSION_MODE` raises it, and a write tier needs the
   agent sandbox (bubblewrap, or the Landlock tier, on Linux): without one
   the turn is refused. `ligus doctor` says which tier the box has and why;
   see [The Linux agent sandbox](../../operate/ops/linux-sandbox.md).
   A tool call that fails with a network error may have hit the egress
   allow-list: the chat shows a note. For dependency installs set
   `LIGUS_SANDBOX_EGRESS_PRESETS=package-registries`; for other hosts use
   `LIGUS_SANDBOX_EGRESS_ALLOW`.
7. **Is it syncing at all?** Look for `sync: … retrying` lines in the log.
8. **Egress restricted?** In dearth mode or a locked chat, runtimes Ligus
   can't confine (aider, pi, Hermes, Antigravity, Copilot) are refused, with
   a note in the chat.

## Claude Code rate limits and plan windows

Subscription runtimes have usage windows (Claude's five-hour and weekly
limits). Ligus throttles background work before you hit them:

- your own messages, regenerates and proposal actions always run;
- scheduled runs pause from about 70 % of a window;
- workflow stages and agent-to-agent hand-offs pause from about 85 %
  (a hand-off stops with a note; mention the agent again to continue);
- each agent's threshold differs by a few percent.

**Settings → Guards → Plan windows** shows each runtime's window and what is
held back. Claude Code reports its rate-limit status while it works; any
subscription CLI that fails with a usage-limit error marks its window full
until it resets (an hour, when the CLI doesn't say).

If a turn fails with a usage-limit error: wait for the reset shown there,
switch the chat to another runtime, or move the agent to a host with its own
subscription. See [Plan windows](../features/guards.md#plan-windows-staggered-throttles).

## Hygiene sweep surprises

The sweep runs 3 minutes after launch and every 6 hours
(**Settings → Guards → Sweep now** runs it at once).

| You notice | What happened | Get it back |
|---|---|---|
| a turn's worktree is gone | no turn was running in it (left by a crash) | unfinished edits were saved as a patch under `<repo>/.ligus/stopped-turns/`; `git apply` it |
| a deleted or archived chat's checkout is gone | the sweep removes it | the chat's `ligus/chat/<id>` branch is kept |
| a proposal left the Review inbox, marked **stale** | its diff no longer applies to the chat's branch | Review pane → **Restore to the inbox**, or ask the agent to redo it |

A worktree a turn is using, or one parked for a live chat's next turn, is
never touched.

## A subprocess agent failed or hung

- **`spawn claude: No such file or directory`** (or `aider`, `pi`, `agy`,
  `copilot`): the CLI isn't on the app's `PATH`. Ligus merges your login
  shell's `PATH` and common bin dirs at startup; export the CLI's location
  in `.zprofile`/`.zshrc` and relaunch.
- **`pi exited with status 1` / "No API key"**: run `pi login`, or point it
  at a local Ollama with the runtime's Ollama base URL.
- **Antigravity or Copilot not signed in**: run `agy` (Sign in with Google)
  or `copilot login` on the machine that runs the agent; on a headless host
  `COPILOT_GITHUB_TOKEN` can hold a fine-grained token with *Copilot
  Requests*. **Settings → Models & runtimes → Subscription CLIs → Re-scan**.
  See [Configuring a runtime](../getting-started/configuring-a-runtime.md#subscription-clis-google-antigravity-and-github-copilot).
- **The turn hangs**: subprocess agents stop after
  `LIGUS_AGENT_TIMEOUT_SECS` (default 300); streaming models after
  `LIGUS_TURN_IDLE_TIMEOUT_SECS` of silence (300); any turn after
  `LIGUS_TURN_TIMEOUT_SECS` (3600).
- **"refused while egress is restricted"**: the chat is locked down or the
  workspace is in dearth mode. See [Workspace guards](../features/guards.md).

## Can't reach a local runtime (e.g. Ollama)

Runtime HTTP calls come from the Rust backend, so `http://localhost:11434`
works. Check the service (`curl http://localhost:11434/api/tags`) and the URL
in the runtime config. For `pi` → Ollama, set the runtime's **Ollama base
URL**.

## An MCP tool is never offered to a local model

Ollama and `custom` runtimes get at most 16 tools a turn. Ligus logs
`tool cap reached: these tools are NOT offered this turn` with the names. To
have a tool offered, name it in the agent's instructions, a skill or your
message, or disable servers the agent doesn't need. Separately, once a turn
has read project files, MCP calls are refused (the exfiltration guard). See
[MCP servers](../addons/mcp.md#local-models-at-most-16-tools-a-turn).

## Agent says it did something but nothing happened

- The runtime may not support Ligus's tool calls (e.g. plain Ollama).
- A subprocess agent (`claude`, `pi`, `aider`) may have run tools through
  its own mechanism: check the chat's worktree and `git log`.
- Edits arrive as a proposal; they reach your working tree only after
  **Implement**.

## Tests fail after pulling main

```bash
cargo test --workspace      # Rust
cd web && bun run build     # frontend typecheck + build
```

## Questions, ideas and bug reports

Ask on GitHub: open an [issue](https://github.com/langstroth-ai/ligus/issues)
for a bug, or start a [discussion](https://github.com/langstroth-ai/ligus/discussions)
for a question or an idea. Relay matters go to
[langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay/issues)
(issues) or its [discussions](https://github.com/langstroth-ai/langstroth-relay/discussions).
Report security problems privately, as `SECURITY.md` in the repository
describes.

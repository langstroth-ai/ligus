# Running ligus serve

`ligus serve` is the always-on Ligus box your [phone](../../ligus/features/phone-app.md)
talks to. It joins the workspace as **its own service member**: an Admin
admits it once with a passkey, and from then on it signs as itself, never as
you. See [Admitting a box](admitting-a-box.md).

This page covers what that means for the phone, how the box's online status
works, and applying approved changes. For reaching the box from the phone, see
[Remote access for your phone](../networking/remote-access.md).

## The box is its own member

| | The box (a service member) |
|---|---|
| Phone messages show as | you, when the phone signs them with its own key ([phone actions](../reference/serve-api.md#phone-actions)); the box (`LIGUS_SERVE_NAME`, else its label, else its host name) for the older `POST /v1/chats/{id}/messages` |
| Role | at most Contributor, never raised above it |
| Votes and review notes | the phone's vote, signed with its own key, counts as yours; the box never votes from the API (`/v1/proposals/{id}/approve` and `/reject` answer 410) |
| Chats the phone creates | owned by the box |
| Who signs events | the box's own account and device key |
| What it can't do | admit, remove or demote anyone, carry a passkey, answer an agent's question, clear a lockdown, apply a change on its own (it applies only when a person asks it to: [Apply on a host](#apply-on-a-host)) |

The box's keys never leave it. Your account key never leaves your desktop.

!!! warning "`ligus serve enroll` and `LIGUS_SERVE_OWNER` were removed"
    Earlier previews could make the box one of your own devices, so it signed
    as you. Since 0.1 both stop with a migration error that points to
    [Admitting a box](admitting-a-box.md). For what only a person may do from
    the phone, the phone signs with its own device key (and your passkey
    where the rules need it), and the box only drafts and relays it: box
    admissions and the [phone actions](../reference/serve-api.md#phone-actions)
    (post, vote, answer, clear a lockdown, Apply on ‹host›).

### Revoke it

A box is removed like any member below Admin, by an Admin with a passkey;
its device can also be revoked on its own. Every device then refuses what it
signs, the box goes read-only (`identity.mode` `revoked`), and the next key
rotation leaves it out. To bring it back, start it from a fresh data dir and
admit it again.

## Securing the box {#securing-the-box}

- **Prefer the relay tunnel, or a tailnet address, over a LAN bind.** The
  direct path is plain HTTP with one bearer token. On a tailnet (Tailscale,
  Headscale) WireGuard encrypts it; on a LAN anyone on the network can read
  the token as it passes. With the tunnel only, keep
  `LIGUS_SERVE_LISTEN=127.0.0.1:8787`.
- **`LIGUS_SERVE_ALLOW_ANY_BIND=1` turns the bind guard off.** The box then
  listens on whatever you name, `0.0.0.0` or a public address included, and
  anyone who can reach the port and learns the token can act as the box.
  Set it only where something else decides who connects (a container's
  private network namespace behind a NetworkPolicy, a firewall).
- **Don't put `--pair` in the service.** `ligus serve --pair` prints the
  pairing link, `serve.token` included on the direct path, and under systemd
  that goes to journald and stays there. Run `ligus pair` from a terminal.
- **Revoking a direct-path phone means rotating `serve.token`.** Every phone
  on the direct path shares it, so all of them pair again; tunnel phones are
  revoked one by one. See [Lost phone](../security/runbooks.md#lost-phone).
- **File modes.** `ligus enroll` writes `config.toml` (the workspace key and
  relay token) `0600` and keeps the data dir `0700`; key seeds are `0600` in
  a `0700` key dir. The shipped unit also sets `UMask=0077`.
- **Agents on the box don't see Ligus's secrets.** Agent CLIs, MCP servers,
  `run_command` and pre-flight tests start with an allow-listed environment
  (no workspace key, relay or tunnel tokens, ntfy tokens, `LIGUS_WS_SECRET_*`,
  and no provider key unless that runtime was given one in Ligus), and run
  in the [OS sandbox](../../ligus/concepts/security.md#os-sandbox): bubblewrap,
  Landlock and seccomp on Linux. The data dir, `serve.token` (also a
  `--token-file` elsewhere), `~/.ssh` and the other credential dirs are
  unreadable; writes land only in the turn's worktree; the network is a
  fresh namespace whose only exit is the turn's egress proxy. So an agent
  can't read the token, and can't reach the serve API on loopback either.
  Under the [Landlock tier](linux-sandbox.md) the same paths are denied by
  Landlock rules, and TCP is limited to the egress proxy's port (by port,
  not address: see the residuals there), so the serve API's port is out of
  reach too.
- **Install bubblewrap.** The strongest Linux sandbox needs `bwrap` and
  unprivileged user namespaces. Where bubblewrap can't create them, the box
  falls back to the [Landlock tier](linux-sandbox.md) (Linux 6.7+:
  Landlock + seccomp, no namespaces, with known gaps); with neither it
  refuses every CLI agent turn, read-only ones too, and every command and
  stdio MCP server, and HTTP models answer without local tools. Run `ligus doctor` to see
  the tier. On **Ubuntu 24.04** AppArmor blocks bubblewrap's user namespace
  (`kernel.apparmor_restrict_unprivileged_userns=1`, and no bwrap profile
  ships before 24.10): the box uses the Landlock tier until you install the
  [optional AppArmor profile](linux-sandbox.md#apparmor-profile). The
  [container image](../deploy/kubernetes.md) ships `bwrap`.
- **HOME is not the data dir.** Agent CLIs keep their logins under `$HOME`;
  run the service with a `HOME` outside `LIGUS_DATA_DIR` (the container image
  and chart use `/home/ligus`), because the data dir is unreadable to them.
- **The box never signs as you.** It is its own service member: its votes
  are its own and never count as a person's approval. What only a person may
  do is signed by the phone's own device key with your passkey, and the box
  only relays it.
- **Keys follow rotations.** On start the box publishes its key-agreement
  key on the workspace config log, so the next rotation is sealed to it; it
  reads and writes at the newest key version. Its secrets (device keys,
  passphrase, relay token) are in `systemd-creds` or your keychain, not in
  `config.toml`.

## Host liveness

Hosts (the `ligus serve` box, `ligus worker` boxes, desktops) show as online or
offline in the phone's Hosts list, agent host badges, the desktop's Hosts
list, `ligus queue` and chat details. The same signal decides whether a desktop
defers an agent's turn to its host.

Liveness rides the relay's **presence channel**, not the event log.
Each running host sends a beat every 30 seconds to an ephemeral slot in the
room, sealed under the workspace key so the relay sees only ciphertext. A
host is online when it has beaten within the last 90 seconds.

The durable host record is written only when something real changes (a new
host, or a new label, kind or owner), and a restart writes nothing.

### The workspace coordinator

Since 0.1 the box also holds the workspace's **coordinator lease**: a signed
claim on the workspace config log, renewed by a heartbeat signed with the
box's device key on the same presence channel. If a second `ligus serve` box
serves the same workspace, it leaves the role to the first while that one
beats, and takes it over (with a higher lease epoch) only after the holder has
been silent for two lease periods (three minutes). `GET /v1/guards` reports
the current holder and epoch, along with dearth mode and locked chats. See
[Workspace guards](../../ligus/features/guards.md).

### What the phone reads from the box (0.1)

| Endpoint | What |
|---|---|
| `GET /v1/proposal-sets`, `GET /v1/proposal-sets/{id}`, `GET /v1/chats/{id}/proposal-sets` | Proposal sets with standings, posted assessments and pre-flight results (read-only; a vote on an alternative is the `vote` [phone action](../reference/serve-api.md#phone-actions)) |
| `GET /v1/guards` | Dearth mode, locked chats, the coordinator, and whether this box's identity may clear a lockdown |
| `POST /v1/chats/{id}/lockdown/clear` | Gone (410): the phone clears a lockdown with the `clearLockdown` [phone action](../reference/serve-api.md#phone-actions) and your passkey |
| `GET /v1/flow` | Board, review queue and alert, marks, routing summary |
| `GET /v1/chats/{id}/handoffs` | A chat's hand-off cards |

Every route needs the pairing token (or the relay tunnel) like the rest of the
API, and acts at the box identity's real roster role. A reviewer recruit that
names you pushes `review.recruit` to paired phones with push on, honouring each
phone's previews setting. The full contract is in `docs/ios-app.md`.

## Apply on a host {#apply-on-a-host}

<a id="apply-on-approve"></a>
There is no auto-apply: approving a proposal (from the phone or the desktop)
records a vote and nothing else, and `LIGUS_SERVE_APPLY_ON_APPROVE` is gone.
A person asks **one** host to apply an approved proposal: **Apply on ‹host›**
on the phone (the `requestApply` phone action) or on the desktop's Review
pane. That writes a signed `proposalApplyRequested` naming the host's device.

- Only the named host applies it, and only once. A `ligus serve` box and a
  `ligus worker` check for requests naming them on every worker tick; the
  desktop checks after each sync. Each host claims the request in its own
  store (`apply_attempts`) before touching git, so a restart or a second
  tick never applies it twice.
- The host first judges the `proposalApplied` it would write as every fold
  will (the request withdrawn, the proposal no longer approved, a human gate
  or a held vote all stop it here), then applies the patch in the chat's
  worktree of its checkout (`ligus/chat/<chat id>`), on the base the person
  saw when the request names one, and commits with a
  `Ligus-Apply-Request: <request id>` trailer. The worktree must be clean:
  anything an agent turn left uncommitted there would ride along in the
  commit, so the host reports the request failed instead ("commit or
  discard them, then ask again"). A box without a git identity
  commits as `Ligus <ligus@localhost>`.
- It reports back: `proposalApplied` with the commit, base and tree, or
  `proposalApplyFailed` with the reason (no checkout for the chat, the
  checkout moved, the patch doesn't apply, not a file change). A failed
  request closes, and a person may ask again, on this host or another.
- One applier per proposal: while a request is open, nobody else applies the
  proposal and no second request is accepted. The requester (or an Admin)
  withdraws an open request (`proposalApplyWithdrawn`); a host that sees
  the withdrawal doesn't apply it, and one that already committed takes its
  commit back when the fold refuses its report.
- A host that crashed after its commit finds the commit by its trailer on
  restart and reports it, but only when that commit is exactly the approved
  patch on its parent (and the parent is the base the person saw): an agent
  turn in the same worktree can write the trailer too. Otherwise, and when
  it crashed before committing, it reports the request failed ("ask
  again").
- A request names a host, never a phone (a phone applies nothing).

The box applies only file-diff proposals; anything else (a command, a
decision) is reported failed with "implement this proposal from a desktop".

To switch a chat's repo, clone, pull or open a pull request from the phone,
see [Repos on the home host](serve-repos.md).

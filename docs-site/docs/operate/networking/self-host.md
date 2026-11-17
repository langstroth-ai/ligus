# Self-hosting a relay

The Langstroth relay is a small Go service that stores and forwards **end-to-end
encrypted** event envelopes between the devices of a workspace, and
optionally tunnels the phone app to a `ligus serve` box. It never holds a
workspace key, so it can't read what it stores. You run one relay for a
team; every device in a workspace points at the same relay URL and room.

It lives in its own repository,
**[github.com/ligus-ai/relay](https://github.com/ligus-ai/relay)** (MIT).
This page covers relay **v0.3.0**: the binary `ligus-relay` and settings
named `LIGUS_RELAY_*`.

!!! note "Older relay builds use the Hive names"
    Relay builds before v0.2.0 ship the binary as `hive-relay`
    (`./cmd/hive-relay`) and read only `HIVE_RELAY_*` settings. v0.2.0 reads
    both spellings (the `LIGUS_RELAY_*` name wins, and a legacy name logs a
    one-time deprecation warning) and keeps `hive-relay` as an alias. If
    `./cmd/ligus-relay` doesn't exist in your checkout, use the `hive-relay`
    and `HIVE_RELAY_*` names in everything below.

## What the relay holds

| Data | Form |
|---|---|
| Workspace event envelopes | ciphertext (ChaCha20-Poly1305, sealed on the device) plus a dedup key and timestamp |
| Keyring, presence and candidate blobs | opaque blobs |
| Workspace members and invites (membership mode only) | account ids, roles, invite **code hashes** |
| Contacts (v0.3) | random contact ids, the SHA-256 of each write key, records (opaque to the relay, verified by clients) and sealed drops |
| Directory, accounts, devices, friends, inbox (1.x GitHub routes, off by default from v0.3) | GitHub login and id, display name, device public keys, device labels, last seen; inbox bodies are opaque |
| Relay users and tokens (admin API) | **SHA-256 hashes** of tokens |
| Tunnel hosts, revocations, push tokens | host id and public key, revoked phone ids, each phone's APNs device token |
| Your APNs key (if you enable push) | `.p8` file or inline PEM in the relay's environment |

It never holds a workspace key, a device private key, model credentials or
plaintext messages. What it **can** see is metadata: which workspace ids
exist, how many envelopes and how big, when devices connect, IP addresses,
GitHub logins that use the directory, and tunnel timing. See the
[threat model](../security/threat-model.md#the-relay).

## Run it

### Build

```sh
git clone https://github.com/ligus-ai/relay.git && cd relay
go build -o ligus-relay ./cmd/ligus-relay       # Go 1.25+ (see go.mod)
./ligus-relay                                   # listens on 0.0.0.0:8443
curl -s localhost:8443/v1/health                # → ok
```

### Docker

```sh
docker build -t ligus-relay https://github.com/ligus-ai/relay.git
docker run -d --name ligus-relay --restart unless-stopped \
  -p 8443:8443 -v ligus-data:/data -e LIGUS_RELAY_DATA_DIR=/data ligus-relay
curl -s localhost:8443/v1/health                # → ok
```

The root `Dockerfile` is Alpine-based and drops to an unprivileged user; it
defaults `LIGUS_RELAY_DATA_DIR=/data`. No relay image is published to a
registry yet: build it yourself.

### Fly.io

The repository includes `deploy/fly.toml` (port 8443, a volume at `/data`,
health check on `/v1/health`). Its `dockerfile` path is relative to
`deploy/`, so run Fly from the repository root and pass the config:

```sh
fly launch --config deploy/fly.toml --copy-config --no-deploy   # pick a name and region
fly volumes create hive_data --size 1 --region <region>
fly deploy . --config deploy/fly.toml
curl -s https://<app>.fly.dev/v1/health          # → ok
```

### systemd

```ini
# /etc/systemd/system/ligus-relay.service
[Unit]
Description=Ligus relay
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/ligus-relay
EnvironmentFile=/etc/ligus-relay.env
User=ligus-relay
Group=ligus-relay
UMask=0077
Restart=always
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
```

```sh
sudo useradd --system --home /var/lib/ligus-relay --create-home ligus-relay
printf 'LIGUS_RELAY_ADDR=127.0.0.1:8443\nLIGUS_RELAY_DATA_DIR=/var/lib/ligus-relay\n' | sudo tee /etc/ligus-relay.env
sudo chmod 600 /etc/ligus-relay.env
sudo systemctl daemon-reload && sudo systemctl enable --now ligus-relay
curl -s localhost:8443/v1/health                # → ok
```

### Kubernetes

Relay v0.3.0 ships Helm charts for the relay and ligus-auth (`deploy/helm/`); see
[Kubernetes](../deploy/kubernetes.md#relay).

## Listen address

`$PORT` (set by most platforms) → `0.0.0.0:$PORT`; else
`LIGUS_RELAY_ADDR`; else `0.0.0.0:8443`. The server has no command-line
flags; it is configured entirely through environment variables. On SIGTERM
it drains for 10 s and flushes its snapshot.

## TLS

The relay speaks **plain HTTP** and doesn't terminate TLS. Clients require
`https://` (the phone tunnel refuses `http` unless explicitly allowed for
testing), so put TLS in front: the platform's edge (Fly, Render, Cloud Run),
an ingress, or a reverse proxy.

```caddy
# /etc/caddy/Caddyfile
relay.example.com {
    reverse_proxy localhost:8443
}
```

Keep the idle timeout of anything in front above 60 s: SSE and tunnel
connections are long-lived.

### Client addresses behind a proxy {#client-ip}

Rate limits and abuse blocks are kept per client: the TCP peer address, with
IPv6 keyed on its /64. Behind a reverse proxy, an ingress or a load balancer,
every client then shares the proxy's address and one bucket (ten bad pairing
guesses from anyone block pairing for everyone for a minute). Tell the relay
which peers are your proxies:

| Setting | Default | Meaning |
|---|---|---|
| `LIGUS_RELAY_TRUSTED_PROXIES` | empty | comma-separated CIDRs or IPs of your proxies. Only for a peer inside this list does the relay read the forwarding header; from anyone else it ignores every forwarding header |
| `LIGUS_RELAY_CLIENT_IP_HEADER` | `X-Forwarded-For` | the header those proxies set. `X-Forwarded-For` is walked right to left past trusted hops; a single-address header such as `X-Real-IP` also works |

Set `LIGUS_RELAY_TRUSTED_PROXIES` behind **any** proxy or ingress.
`Fly-Client-IP` is used only on Fly.io (when `FLY_APP_NAME` is set and
`LIGUS_RELAY_TRUSTED_PROXIES` isn't), where Fly's edge overwrites it;
anywhere else a client-supplied `Fly-Client-IP` is ignored.

## Storage

| Backend | When | Survives restart | Multiple instances |
|---|---|---|---|
| Postgres | `DATABASE_URL` set | yes | sync routes yes (see [Scaling](#scaling)) |
| Memory + JSON snapshot | `LIGUS_RELAY_DATA_DIR` set | yes: `relay-state.json`, flushed every 5 s and on shutdown, written atomically | no |
| Memory only | neither | **no** | no |

There is no SQLite backend. Set `LIGUS_RELAY_REQUIRE_DB=1` in production to
refuse to start without `DATABASE_URL` rather than silently running on
memory.

**Memory budget.** On the memory and snapshot stores everything lives in
RAM. On any relay untrusted clients can reach (an open relay above all), set
`LIGUS_RELAY_MAX_STORE_BYTES` below the process's memory limit: it caps the
bytes held across all workspaces (envelope bodies, keyring rows, candidate
and presence blobs, per-workspace overhead), and writes past it answer
`507 relay storage is full`. The default `0` is unbounded.

**Postgres.** Use a primary (the relay refuses a read replica). It works
behind PgBouncer. The schema is created and upgraded automatically on every
start with idempotent `CREATE TABLE IF NOT EXISTS` / `ADD COLUMN IF NOT
EXISTS` statements; there is no separate migration step and no down
migration.

### Retention

The relay is a **cache of recent encrypted traffic**, not the system of
record: every device keeps its own full copy. A device that was offline
longer than the retention window catches up from a peer that still has the
events.

| Setting | Default | Applies to |
|---|---|---|
| `LIGUS_RELAY_MAX_ENVELOPES` | `50000` per workspace (`0` = no cap) | memory (on write) and Postgres (pruned hourly to the newest N, so a workspace can briefly exceed it) |
| `LIGUS_RELAY_RETENTION_DAYS` | `0` (off) | memory and Postgres |
| `LIGUS_RELAY_INBOX_MAX_ROWS` / `LIGUS_RELAY_INBOX_RETENTION_DAYS` | `5000` / `30` | account inbox |
| `LIGUS_RELAY_MAX_KEYRING_PER_WS` | `2000` | key rotations per workspace; past it a new rotation gets `507` and stored ones are never evicted |
| `LIGUS_RELAY_MAX_KEYRING_BYTES_PER_WS` | 16 MiB | keyring bytes per workspace (`507` past it); keep it at or below `LIGUS_RELAY_MAX_LIST_BYTES` |
| `LIGUS_RELAY_MAX_ROTATION_BYTES` | 256 KiB | one key rotation (`413` past it) |
| `LIGUS_RELAY_MAX_DEVICE_BLOBS_PER_WS` | `1000` | device blobs |
| `LIGUS_RELAY_FRIEND_REQ_RETENTION_DAYS` | `30` | friend requests |
| `LIGUS_RELAY_MAX_PAIRINGS` | `50000` | pending pairing codes |

Postgres does **not** mean unbounded history: set
`LIGUS_RELAY_MAX_ENVELOPES=0` if that's what you want.

## Access control

The relay has independent layers. Pick the ones you need; the content is
end-to-end encrypted whatever you choose.

### Access tokens: who may use the relay at all

Chosen in this order:

| Mode | Set | Behaviour |
|---|---|---|
| **Signed tokens** | `LIGUS_RELAY_TOKEN_PUBKEY=<hex or base64 Ed25519 public key>` | requests need `Authorization: Bearer hrt1.<claims>.<sig>` signed by your issuer key; expiry is enforced |
| **Static allowlist** | `LIGUS_RELAY_ACCESS_TOKENS=tokA,tokB` | requests need one of those bearer tokens (stored hashed, compared in constant time). Changing the list needs a restart |
| **Open** (default) | neither | anyone who knows the URL may use it |

The gate covers reads and writes on every workspace route. It doesn't cover
`/`, `/v1/health`, `/metrics` (see [Monitoring](#monitoring)),
`/v1/admin/*` or `/v1/tunnel/*` (the tunnel has its own checks). A failed
check is `401 relay requires a valid access token`.

Mint signed tokens with the relay binary, **on a machine other than the
relay**:

```console
$ ligus-relay keygen
# Ed25519 issuer keypair
private_key=3f9c…   # SECRET — keep on the issuer only (never on the relay)
public_key=a1b2…    # set as HIVE_RELAY_TOKEN_PUBKEY on the relay
$ ligus-relay issue --key 3f9c… --sub ligus:6f1c2d3e-8a7b-4c1d-9e0f-112233445566 --exp-days 365
hrt1.eyJzdWIiOiJnaXRodWI6YWxpY2UiLC…
```

(`LIGUS_RELAY_TOKEN_PUBKEY` and `HIVE_RELAY_TOKEN_PUBKEY` are the same
setting.) `issue` takes `--sub <id>` and optionally `--exp-days N` (no
expiry if omitted), `--workspace <id>` (see below), `--cap X`, and the
informational `--plan`, `--max-members N`, `--retention-days N` and `--turn`
(see the claims table below). `ligus-relay bootstrap-agent`
mints a token for a headless agent and prints the one-line
`ligus enroll … && ligus worker` command; see
[Headless agents](../concepts/headless-agents.md#3-remote-agent-always-on-worker).
Its output contains the workspace key, so treat it as a secret.

!!! danger "A malformed public key stops the relay"
    Since v0.2.0, if `LIGUS_RELAY_TOKEN_PUBKEY` is set but isn't an Ed25519
    public key (64 hex characters or base64), the relay refuses to start.
    (Earlier versions fell through to the allowlist or **open** mode.) After
    setting it, check that an unauthenticated request is refused:

    ```sh
    curl -s -o /dev/null -w '%{http_code}\n' https://relay.example.com/v1/workspaces/x/envelopes
    # 401
    ```

Each client sets its token in **Settings → Team sync → Relay access token**
(desktop), `ligus enroll … --token` or `LIGUS_RELAY_ACCESS_TOKEN` (CLI).

#### The `hrt1` token format

A signed token is three `.`-separated parts:

```text
hrt1.<b64url(claims_json)>.<b64url(ed25519_sig)>
```

The signature covers `hrt1.<b64url(claims_json)>`, so the claims can't be
edited without the issuer key. The relay holds only the public key: it can
verify tokens but never mint them.

| Claim | Meaning |
|---|---|
| `sub` | the account or device the token identifies (relay-side membership keys on it) |
| `exp` | unix-seconds expiry (`0` = never); short expiries plus reissue are the revocation story |
| `workspaces` | optional workspace ids the token is confined to (see [Per-workspace token scope](#per-workspace-token-scope)) |
| `caps` | capability strings; forward-compatible, the relay ignores ones it doesn't act on |
| `plan`, `max_members`, `retention_days`, `turn` | informational fields kept for wire compatibility; the relay doesn't act on them |

ligus-auth adds `tid`, `uid` and `did`; see [ligus-auth](../deploy/ligus-auth.md).

### Per-workspace token scope {#per-workspace-token-scope}

The token gate is coarse: by default any admitted caller may read and write
any workspace whose id it knows. Signed tokens can carry a `workspaces`
claim (`ligus-relay issue … --workspace <id> [--workspace <id>]`, `*` = any),
and `LIGUS_RELAY_WORKSPACE_SCOPE` decides what the relay does with it:

| Value | Behaviour |
|---|---|
| `off` (default) | the claim is ignored |
| `claims` | a token that carries the claim is confined to those workspace ids on every `/v1/workspaces/{id}/…` route (envelopes, events, keyring, presence, candidates, membership); a token without it keeps coarse access |
| `strict` | every workspace route needs a token with the claim; the open policy and the static allowlist get `403`, so this needs signed tokens |

The claim sits inside the signed claims, so the `hrt1` format is unchanged,
but a relay that doesn't enforce it (older, or `off`) ignores it. Scoping
tokens also narrows who can fill a workspace's keyring route. Rotations are
signed and devices never seal under a keyring row, but junk rows can use up
the relay's keyring cap; newcomers then get the keys from their invite.
Relay-side membership (below) is the alternative when the roster, not the
issuer, should decide.

### Membership: who may use a given workspace

`LIGUS_RELAY_MEMBERSHIP=1` turns on relay-side membership: a workspace that
has been **claimed** keeps a roster and invite codes on the relay, and only
its members may read or write it. Unclaimed workspaces are unaffected. The
relay needs to know who is calling: either a signed token (its `sub`) or
`LIGUS_RELAY_MEMBERSHIP_PUBKEY`, which reads identity from an `hrt1` token
while leaving relay access open. See
[Server-side membership](../ops/server-side-membership.md).

### The user and token admin API

`/v1/admin/*` (create users, issue and revoke tokens) is **disabled in the
open-source binary**: it answers 404 unless the relay is built with an
admin authorizer in code. No environment variable enables it. Ligus has no
UI for it: operators issue relay access tokens on the relay side.

### GitHub-backed routes (1.x only)

Ligus 0.1 doesn't use these routes and never sends a GitHub (or any forge)
token to a relay. From relay v0.3 they are off unless the operator turns them
on for 1.x clients. Directory, account and friends routes identify the caller by a GitHub token
sent in `X-Hive-Github-Token` (or `X-Ligus-Github-Token`), verified against
`api.github.com` and cached briefly. Only that header is used: the
`Authorization` bearer carries the relay access token and is never sent to
GitHub. The relay operator can see the GitHub tokens clients send here,
since the relay must present them to GitHub; use these routes only with a
relay you trust. The relay needs outbound HTTPS to `api.github.com` for them.

## The phone tunnel and push

Off by default.

| Setting | Values |
|---|---|
| `LIGUS_RELAY_TUNNEL` | `off` (default; `/v1/tunnel/*` answers 404), `open` (any host may register), `allowlist`. Use `open` only together with an access gate, so that a host's first registration needs a token; otherwise anyone can relay phone sessions through you |
| `LIGUS_RELAY_TUNNEL_HOSTS` | comma-separated host ids for `allowlist` (from `ligus remote status`). `allowlist` with no hosts leaves the tunnel off |

If an access gate is configured, a host must present a token that
passes it to register (`ligus remote enable --reg-token`). The tunnel's
control connections live in one process, so the tunnel must run on **one
instance**.

APNs push (only with the tunnel on):

| Setting | Meaning |
|---|---|
| `LIGUS_RELAY_APNS_KEY_FILE` or `LIGUS_RELAY_APNS_KEY` | the `.p8` key, as a file or inline PEM (`\n` escapes allowed) |
| `LIGUS_RELAY_APNS_KEY_ID`, `LIGUS_RELAY_APNS_TEAM_ID` | Key ID and Apple Team ID |
| `LIGUS_RELAY_APNS_TOPIC` | the app's bundle id |
| `LIGUS_RELAY_APNS_ENV` | `auto` (default; what each phone reports), `sandbox`, `production` |

A partial or bad configuration logs an error and leaves push off; the relay
still starts. What Apple sees: a generic alert ("New activity on your host")
plus an encrypted blob only the phone can open. Push only works for an app
build signed by the same Apple team as the key. Step by step:
[Remote access → Push through your own relay](remote-access.md#push-through-your-own-relay).

```sh
curl -s https://relay.example.com/v1/tunnel/info
# {"versions":[1],"push":true,"billing":"none"}   (404 with the tunnel off)
```

## Rate and size limits

| Setting | Default | What it limits |
|---|---|---|
| `LIGUS_RELAY_MAX_BODY_BYTES` | 4 MiB | request body (413 above) |
| `LIGUS_RELAY_MAX_LIST_BYTES` | 16 MiB (`0` = unbounded) | one `GET …/keyring`, `…/candidates` or `…/presence` response, by stored bytes; at least one row is always returned |
| `LIGUS_RELAY_PUSH_RATE` / `LIGUS_RELAY_PUSH_BURST` | 100/s, burst 300 | writes per client |
| `LIGUS_RELAY_READ_RATE` / `LIGUS_RELAY_READ_BURST` | 100/s, burst 400 | workspace reads and SSE connects per client (429 above) |
| `LIGUS_RELAY_WS_WRITER_RATE` / `LIGUS_RELAY_WS_WRITER_BURST` | 20/s, burst 200 | writes per workspace |
| `LIGUS_RELAY_GITHUB_RATE` / `LIGUS_RELAY_GITHUB_BURST` | 30/s, burst 120 | GitHub-verified routes per IP |
| `LIGUS_RELAY_INVITE_RATE` / `LIGUS_RELAY_INVITE_BURST` | 0.05/s, burst 10 | invite redemption |
| `LIGUS_RELAY_INVITE_REQUIRE_FRIEND` | off | only friends may redeem invites |
| `LIGUS_RELAY_MAX_SSE` | 2000 | concurrent SSE streams per instance (503 above) |
| `LIGUS_RELAY_MAX_SSE_PER_CLIENT` | 100 | concurrent SSE streams per client (429 above) |
| `LIGUS_RELAY_FRIEND_CAP` | unlimited | friends per account |
| pairing codes | 10 failed lookups per minute per IP, then 429 | fixed |
| tunnel | 8 sessions per host, 30 connects per IP per minute | fixed |

Envelope pages are at most 1000 envelopes or 4 MiB. The relay sends no CORS
headers. A "client" is the token subject where one applies, else the address
(see [Client addresses behind a proxy](#client-ip)).

Since v0.2.0 the relay also answers:

| Status | When |
|---|---|
| `400` | a workspace id longer than 512 bytes, a device id longer than 256 bytes, or a malformed JSON body |
| `507` | the memory store is at `LIGUS_RELAY_MAX_STORE_BYTES`, or a workspace's keyring is at its rotation or byte cap |
| `413` | a body over `LIGUS_RELAY_MAX_BODY_BYTES`, or a key rotation over `LIGUS_RELAY_MAX_ROTATION_BYTES` |

Pairing codes take only payloads safe for the operator to read (a peer
friend code or a `sealed1:` payload; a workspace invite gets `422`) and are
single-use.

## Monitoring

| Endpoint | Auth | Use |
|---|---|---|
| `GET /v1/health` | none | liveness **and** readiness: round-trips the store (cached 1 s); `200 ok` or `503` |
| `GET /metrics` | open, or bearer `LIGUS_RELAY_METRICS_TOKEN` | Prometheus: `hive_relay_requests_total`, `hive_relay_requests_by_class`, `hive_relay_requests_in_flight` |
| `GET /` | none | a small HTML status page |

Set `LIGUS_RELAY_METRICS_TOKEN` on any relay reachable from the internet.
Logs go to stdout/stderr.

## Scaling

- **Memory or snapshot store: one instance.** Don't scale out.
- **Postgres:** the sync routes can run on several instances. The tunnel
  can't: run it on exactly one (the Helm chart's `split` topology does this,
  routing `/v1/tunnel` to a single tunnel pod).
- Pairing codes, SSE fan-out and rate limiters are per process.

## Backups

A relay holds no data that only it has: every device keeps the full log of
the workspaces it belongs to. Losing the relay loses queued traffic for
devices that were offline longer than peers kept serving it, relay users and
tokens, membership rosters (if used) and phone tunnel registrations. Back it
up for convenience, not as your only copy.

- **Snapshot store:** copy `relay-state.json` from the data dir. It is
  replaced atomically, so a copy is always consistent.

    ```sh
    install -d -m 700 /var/backups/ligus-relay
    cp /var/lib/ligus-relay/relay-state.json /var/backups/ligus-relay/relay-state-$(date +%F).json
    ```

- **Postgres:** `pg_dump` on your normal schedule.

    ```sh
    pg_dump --format=custom "$DATABASE_URL" > relay-$(date +%F).dump
    ```

- **Restore:** stop the relay, put the snapshot back (or `pg_restore`),
  start it. Devices resume from their own cursors.
- The snapshot file is written owner-only (`0600`) since v0.2.0; keep the
  directory private (`0700`) too, since it holds hashed tokens, push tokens
  and metadata.

## Upgrades

1. Read the release notes in the relay repository.
2. Back up (above).
3. Replace the binary or image and restart. Postgres schema changes apply on
   start. The wire protocol (`/v1`, `hrt1` tokens) is kept compatible, so
   clients don't need to upgrade in step.
4. Check `curl -s https://relay.example.com/v1/health` → `ok`, and that a
   device's sync pill reads **live**.

To roll back, run the previous binary against a restored backup; schema
changes have no down migration.

## Checklist for an internet-facing relay

- [ ] TLS in front; `LIGUS_RELAY_TRUSTED_PROXIES` set to your proxy's addresses
- [ ] `DATABASE_URL` or `LIGUS_RELAY_DATA_DIR` set; `LIGUS_RELAY_REQUIRE_DB=1` with Postgres
- [ ] `LIGUS_RELAY_MAX_STORE_BYTES` set on a memory or snapshot store
- [ ] an access gate (`LIGUS_RELAY_TOKEN_PUBKEY` or `LIGUS_RELAY_ACCESS_TOKENS`) unless you want a public relay, verified with an unauthenticated request
- [ ] `LIGUS_RELAY_WORKSPACE_SCOPE=claims` (or `strict`) with workspace-scoped signed tokens
- [ ] issuer private key kept off the relay host
- [ ] `LIGUS_RELAY_METRICS_TOKEN` set
- [ ] tunnel `off` unless you use the phone app; `allowlist` if only your boxes should register, `open` only with an access gate
- [ ] backups scheduled

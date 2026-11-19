# Self-hosting a relay

The Langstroth relay is a small Go service that stores and forwards **end-to-end
encrypted** event envelopes between the devices of a workspace, and
optionally tunnels the phone app to a `ligus serve` box. It never holds a
workspace key, so it can't read what it stores. You run one relay for a
team; every device in a workspace points at the same relay URL and room.

It lives in its own repository,
**[github.com/langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay)** (MIT).
This page covers relay **v0.3.0**: the binary `langstroth-relay` and settings
named `LANGSTROTH_RELAY_*`.

## What the relay holds

| Data | Form |
|---|---|
| Workspace event envelopes | ciphertext (ChaCha20-Poly1305, sealed on the device) plus a dedup key and timestamp |
| Keyring, presence and candidate blobs | opaque blobs |
| Shared files (v0.3, [blob channel](#file-attachments)) | ciphertext sealed on the device, its size, the uploader's token subject (or address) and the upload and expiry times |
| Workspace members and invites (membership mode only) | account ids, roles, invite **code hashes** |
| Contacts (v0.3) | random contact ids, the SHA-256 of each write key, records (opaque to the relay, verified by clients) and sealed drops |
| Directory, accounts, devices, friends, inbox (GitHub routes, off by default from v0.3) | GitHub login and id, display name, device public keys, device labels, last seen; inbox bodies are opaque |
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
git clone https://github.com/langstroth-ai/langstroth-relay.git && cd langstroth-relay
git checkout v0.3.0
go build -o langstroth-relay ./cmd/langstroth-relay       # Go 1.27+ (see go.mod)
./langstroth-relay                                   # listens on 0.0.0.0:8443
curl -s localhost:8443/v1/health                # → ok
```

### Docker

Build the image from the repository:

```sh
docker build -t langstroth-relay https://github.com/langstroth-ai/langstroth-relay.git#v0.3.0
docker run -d --name langstroth-relay --restart unless-stopped \
  -p 8443:8443 -v langstroth-data:/data -e LANGSTROTH_RELAY_DATA_DIR=/data langstroth-relay
curl -s localhost:8443/v1/health                # → ok
```

The root `Dockerfile` is Alpine-based: it starts as root to `chown` the data
volume, then drops to the unprivileged `langstroth` user (UID 1000), and it
defaults `LANGSTROTH_RELAY_DATA_DIR=/data`.

Relay releases also publish images, `ghcr.io/langstroth-ai/langstroth-relay:<version>`
(linux/amd64 and linux/arm64), built from `deploy/container/Dockerfile`: a
distroless image with no shell or entrypoint script that runs the binary as
UID 65532. It doesn't default the data dir and can't `chown` a volume, so
set `LANGSTROTH_RELAY_DATA_DIR` yourself and make the volume writable by
65532 (or use `DATABASE_URL`):

```sh
docker volume create langstroth-data
docker run --rm -v langstroth-data:/data alpine chown 65532:65532 /data
docker run -d --name langstroth-relay --restart unless-stopped \
  -p 8443:8443 -v langstroth-data:/data -e LANGSTROTH_RELAY_DATA_DIR=/data \
  ghcr.io/langstroth-ai/langstroth-relay:0.3.0
```

### Fly.io

The repository includes `deploy/fly.toml` (port 8443, a volume at `/data`,
health check on `/v1/health`). Its `dockerfile` path is relative to
`deploy/`, so run Fly from the repository root and pass the config:

```sh
fly launch --config deploy/fly.toml --copy-config --no-deploy   # pick a name and region
fly volumes create hive_data --size 1 --region <region>   # the name deploy/fly.toml mounts
fly deploy . --config deploy/fly.toml
curl -s https://<app>.fly.dev/v1/health          # → ok
```

### systemd

```ini
# /etc/systemd/system/langstroth-relay.service
[Unit]
Description=Langstroth relay
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/langstroth-relay
EnvironmentFile=/etc/langstroth-relay.env
User=langstroth-relay
Group=langstroth-relay
UMask=0077
Restart=always
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
```

```sh
sudo useradd --system --home /var/lib/langstroth-relay --create-home langstroth-relay
printf 'LANGSTROTH_RELAY_ADDR=127.0.0.1:8443\nLANGSTROTH_RELAY_DATA_DIR=/var/lib/langstroth-relay\n' | sudo tee /etc/langstroth-relay.env
sudo chmod 600 /etc/langstroth-relay.env
sudo systemctl daemon-reload && sudo systemctl enable --now langstroth-relay
curl -s localhost:8443/v1/health                # → ok
```

### Kubernetes

Relay v0.3.0 ships Helm charts for the relay and langstroth-auth
(`deploy/helm/langstroth-relay`, `deploy/helm/langstroth-auth`, and the
`langstroth-postgres` library); releases publish them at
`oci://ghcr.io/langstroth-ai/charts/<name>`. `database.mode` picks the
Postgres (`external`, `cnpg` or `bundled`). See
[Kubernetes](../deploy/kubernetes.md#relay).

### With a coding agent

`AGENTS.md` in the relay repository has a decision table (a local binary, an
in-memory evaluation install, or a team install with bundled, external or
CloudNativePG Postgres) and non-interactive scripts in `deploy/agent/`
(`local.sh`, `preflight.sh`, `install.sh`, `verify.sh`, `teardown.sh`) that
print JSON with `--json`. See also [Deploying with an agent](../deploy/with-an-agent.md).

## Listen address

`$PORT` (set by most platforms) → `0.0.0.0:$PORT`; else
`LANGSTROTH_RELAY_ADDR`; else `0.0.0.0:8443`. The server has no command-line
flags; it is configured entirely through environment variables (the
`keygen`, `issue` and `bootstrap-agent` subcommands are tools, not server
options). On SIGTERM it drains for 10 s and flushes its snapshot.

### Settings from files

Any setting `X` (and `DATABASE_URL`) that isn't in the environment is read
from the file named by `X_FILE` (for example `DATABASE_URL_FILE`), else from
`$LANGSTROTH_RELAY_SECRETS_DIR/X`, so secrets can be mounted files. The
environment wins over `X_FILE`, which wins over the directory; one trailing
newline is trimmed. The relay refuses to start when a file it is pointed at
can't be read or is empty, since an unreadable `LANGSTROTH_RELAY_TOKEN_PUBKEY`
would otherwise mean an open relay. (`LANGSTROTH_RELAY_APNS_KEY_FILE` keeps its
own meaning: the path of the APNs key.)

Settings are read only under `LANGSTROTH_RELAY_*` names. The relay refuses to
start while a `HIVE_RELAY_*` or `LIGUS_RELAY_*` variable is set (the Ligus
client's `LIGUS_RELAY_URL` excepted) and names the variable to use instead.

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
| `LANGSTROTH_RELAY_TRUSTED_PROXIES` | empty | comma-separated CIDRs or IPs of your proxies. Only for a peer inside this list does the relay read the forwarding header; from anyone else it ignores every forwarding header |
| `LANGSTROTH_RELAY_CLIENT_IP_HEADER` | `X-Forwarded-For` | the header those proxies set. `X-Forwarded-For` is walked right to left past trusted hops; a single-address header such as `X-Real-IP` also works |

Set `LANGSTROTH_RELAY_TRUSTED_PROXIES` behind **any** proxy or ingress.
`Fly-Client-IP` is used only on Fly.io (when `FLY_APP_NAME` is set and
`LANGSTROTH_RELAY_TRUSTED_PROXIES` isn't), where Fly's edge overwrites it;
anywhere else a client-supplied `Fly-Client-IP` is ignored.

## Storage

| Backend | When | Survives restart | Multiple instances |
|---|---|---|---|
| Postgres | `DATABASE_URL` set | yes | sync routes yes (see [Scaling](#scaling)) |
| Memory + JSON snapshot | `LANGSTROTH_RELAY_DATA_DIR` set | yes: `relay-state.json`, flushed every 5 s and on shutdown, written atomically | no |
| Memory only | neither | **no** | no |

There is no SQLite backend. Set `LANGSTROTH_RELAY_REQUIRE_DB=1` in production to
refuse to start without `DATABASE_URL` rather than silently running on
memory.

**Memory budget.** On the memory and snapshot stores everything lives in
RAM. On any relay untrusted clients can reach (an open relay above all), set
`LANGSTROTH_RELAY_MAX_STORE_BYTES` below the process's memory limit: it caps the
bytes held across all workspaces (envelope bodies, keyring rows, candidate
and presence blobs, per-workspace overhead), and writes past it answer
`507 relay storage is full`. The default `0` is unbounded.

**Postgres.** Use a primary (the relay refuses a read replica). It works
behind PgBouncer. The schema is created and upgraded automatically on every
start with idempotent `CREATE TABLE IF NOT EXISTS` / `ADD COLUMN IF NOT
EXISTS` statements; there is no separate migration step and no down
migration.

### Retention {#retention}

The relay **never removes envelopes from a workspace it still holds**. A
Ligus device rebuilds a workspace by folding its whole envelope log from the
founder's signed first event, so trimming the front of a log would break
every device that joins afterwards. The per-envelope bounds
`LANGSTROTH_RELAY_MAX_ENVELOPES` and `LANGSTROTH_RELAY_RETENTION_DAYS` are
removed: since v0.3.0 the relay **refuses to start** while either is set to
anything but `0`. Bound storage instead with the quotas in
[Community limits](#community-limits), which refuse new writes (`507`)
rather than evict, `LANGSTROTH_RELAY_MAX_STORE_BYTES` on the memory store,
and whole-room expiry (`LANGSTROTH_RELAY_ROOM_RETENTION_DAYS`).

Other stored data is bounded like this:

| Setting | Default | Applies to |
|---|---|---|
| `LANGSTROTH_RELAY_INBOX_MAX_ROWS` / `LANGSTROTH_RELAY_INBOX_RETENTION_DAYS` | `5000` / `30` | account inbox (GitHub routes only) |
| `LANGSTROTH_RELAY_MAX_KEYRING_PER_WS` | `2000` | key rotations per workspace; past it a new rotation gets `507` and stored ones are never evicted |
| `LANGSTROTH_RELAY_MAX_KEYRING_BYTES_PER_WS` | 16 MiB | keyring bytes per workspace (`507` past it); keep it at or below `LANGSTROTH_RELAY_MAX_LIST_BYTES` |
| `LANGSTROTH_RELAY_MAX_ROTATION_BYTES` | 256 KiB | one key rotation (`413` past it) |
| `LANGSTROTH_RELAY_MAX_DEVICE_BLOBS_PER_WS` | `1000` | device blobs |
| `LANGSTROTH_RELAY_FRIEND_REQ_RETENTION_DAYS` | `30` | answered friend requests on Postgres (GitHub routes only) |
| `LANGSTROTH_RELAY_MAX_PAIRINGS` | `50000` | live pairing codes per instance (`503` past it) |
| `LANGSTROTH_RELAY_CONTACT_TTL_DAYS` | `30` | a contact record expires this long after its last write |
| `LANGSTROTH_RELAY_MAX_CONTACTS` | `100000` | live contact records on the memory store (`507` past it) |
| `LANGSTROTH_RELAY_CONTACTS` | `on` | `off` turns the `/v1/contacts/*` routes off (`404`) |

## File attachments {#file-attachments}

Relays from v0.3 serve the blob channel that carries the files members share
([Sharing files](../../ligus/features/attachments.md)). A file is sealed with
the workspace key on the sender's device, so the relay stores ciphertext and
can't see a file's contents, name or type, or scan it. The signed reference to
a file travels in the encrypted workspace log, and every receiving device
checks the downloaded bytes against it, so a relay that swaps, truncates or
replays a file is caught there.

The relay's job is to keep the channel from becoming free storage or a
distribution point: every request is authenticated like the other workspace
routes, blob ids are 32 random bytes the relay mints (there is no listing
route), uploads go only to a workspace that already holds data, and these
limits refuse what goes over them (nothing is evicted):

| Setting | Default | Limits |
|---|---|---|
| `LANGSTROTH_RELAY_BLOBS` | `on` | `off` turns the channel off (`404`); members can then only attach device-local files |
| `LANGSTROTH_RELAY_BLOB_MAX_BYTES` | 25 MiB | one file (`413 blob_too_large`) |
| `LANGSTROTH_RELAY_BLOB_ROOM_MAX_BYTES` | 512 MiB | file bytes per workspace (`507 blob_room_bytes`) |
| `LANGSTROTH_RELAY_BLOB_ROOM_MAX_COUNT` | `2000` | files per workspace (`507 blob_room_count`) |
| `LANGSTROTH_RELAY_BLOB_WRITER_MAX_BYTES` / `…_WRITER_MAX_COUNT` | 128 MiB / `500` | per member (token subject) per workspace (`507 blob_writer_bytes` / `blob_writer_count`) |
| `LANGSTROTH_RELAY_BLOB_TOTAL_MAX_BYTES` | off | file bytes across the relay (`507 blob_total_bytes`) |
| `LANGSTROTH_RELAY_BLOB_RETENTION_DAYS` | `30` | a file expires this long after upload (`0` = kept until deleted) |
| `LANGSTROTH_RELAY_BLOB_RATE` / `…_BURST` | 1/s, burst 20 | uploads per member per workspace (`429 blob_rate`) |
| `LANGSTROTH_RELAY_BLOB_MAX_INFLIGHT` / `…_WRITER_INFLIGHT` | `16` / `4` | uploads being received at once, relay-wide and per uploader (`429 blob_inflight`) |
| `LANGSTROTH_RELAY_BLOB_DOWNLOAD_RATE` / `…_BURST` | 0.5/s, burst 100 | downloads of one file (`429 blob_download_rate`) |
| `LANGSTROTH_RELAY_BLOB_CLIENT_DOWNLOAD_RATE` / `…_BURST` | 2/s, burst 60 | file downloads per downloader (`429 blob_download_rate`) |

With tenancy, a tenant's `limits` can set `blob_max_bytes` and
`blob_max_count` (the tenant's total) and `blob_room_max_bytes` and
`blob_room_max_count` (per workspace, replacing the relay-wide values). In the
Helm chart these are the `blobs.*` values (`enabled`, `maxBytes`,
`roomMaxBytes`, `roomMaxCount`, `writerMaxBytes`, `writerMaxCount`,
`totalMaxBytes`, `retentionDays`, `rate`, `burst`); with
`ingress.streamingDefaults` the chart raises the ingress body limit to
`blobs.maxBytes`. A proxy in front of the relay must accept request bodies of
that size.

Files are removed when they expire, when a member's device deletes one after a
signed deletion (the file's sharer or an Admin), when a member is removed
through relay membership (their uploads), and with a room purge or idle-room
expiry. A file the policy in a Ligus workspace allows can still be larger than
the relay's cap: the relay's cap wins, and the sender sees the refusal.

On the memory store files count toward `LANGSTROTH_RELAY_MAX_STORE_BYTES` and
are saved in the snapshot; size the budget and the volume for them, or use
Postgres (tables `relay_blobs` and `relay_blob_usage`, migrated under the
relay's advisory lock). The relay's own guide, `docs/blobs.md` in the relay
repository, has the full details.

## Access control

The relay has independent layers. Pick the ones you need; the content is
end-to-end encrypted whatever you choose.

### Access tokens: who may use the relay at all

Chosen in this order:

| Mode | Set | Behaviour |
|---|---|---|
| **Signed tokens** | `LANGSTROTH_RELAY_TOKEN_PUBKEY=<hex or base64 Ed25519 public key>` | requests need `Authorization: Bearer hrt1.<claims>.<sig>` signed by your issuer key; expiry is enforced. `LANGSTROTH_RELAY_TOKEN_PUBKEY_NEXT` adds a second accepted key while you rotate the issuer |
| **Static allowlist** | `LANGSTROTH_RELAY_ACCESS_TOKENS=tokA,tokB` | requests need one of those bearer tokens (stored hashed, compared in constant time). Changing the list needs a restart |
| **Open** (default) | neither | anyone who knows the URL may use it |

The gate covers reads and writes on every workspace route. It doesn't cover
`/`, `/v1/health`, `/metrics` (see [Monitoring](#monitoring)),
`/v1/admin/*` or `/v1/tunnel/*` (the tunnel has its own checks). A failed
check is `401 relay requires a valid access token`.

Mint signed tokens with the relay binary, **on a machine other than the
relay**:

```console
$ langstroth-relay keygen
# Ed25519 issuer keypair
private_key=3f9c…   # SECRET — keep on the issuer only (never on the relay)
public_key=a1b2…    # set as LANGSTROTH_RELAY_TOKEN_PUBKEY on the relay
$ langstroth-relay issue --key 3f9c… --sub ligus:6f1c2d3e-8a7b-4c1d-9e0f-112233445566 --exp-days 365
hrt1.eyJzdWIiOiJnaXRodWI6YWxpY2UiLC…
```

`issue` takes `--sub <id>` and optionally `--exp-days N` (no
expiry if omitted), `--workspace <id>` (see below), `--cap X`, and the
informational `--plan`, `--max-members N`, `--retention-days N` and `--turn`
(see the claims table below). `langstroth-relay bootstrap-agent`
mints a token for a headless agent and prints the one-line
`ligus enroll … && ligus worker` command; see
[Headless agents](../concepts/headless-agents.md#3-remote-agent-always-on-worker).
Its output contains the workspace key, so treat it as a secret.

!!! danger "A malformed public key stops the relay"
    Since v0.2.0, if `LANGSTROTH_RELAY_TOKEN_PUBKEY` is set but isn't an Ed25519
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

langstroth-auth adds `tid`, `uid` and `did`; see [langstroth-auth](../deploy/ligus-auth.md).

### Per-workspace token scope {#per-workspace-token-scope}

The token gate is coarse: by default any admitted caller may read and write
any workspace whose id it knows. Signed tokens can carry a `workspaces`
claim (`langstroth-relay issue … --workspace <id> [--workspace <id>]`, `*` = any),
and `LANGSTROTH_RELAY_WORKSPACE_SCOPE` decides what the relay does with it:

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

`LANGSTROTH_RELAY_MEMBERSHIP=1` turns on relay-side membership: a workspace that
has been **claimed** keeps a roster and invite codes on the relay, and only
its members may read or write it. Unclaimed workspaces are unaffected. The
relay needs to know who is calling: either a signed token (its `sub`) or
`LANGSTROTH_RELAY_MEMBERSHIP_PUBKEY`, which reads identity from an `hrt1` token
while leaving relay access open. See
[Server-side membership](../ops/server-side-membership.md).

### The user and token admin API

`/v1/admin/*` (create users, issue and revoke tokens) is **disabled in the
open-source binary**: it answers 404 unless the relay is built with an
admin authorizer in code. No environment variable enables it. Ligus has no
UI for it: operators issue relay access tokens on the relay side.

### GitHub-backed routes (off by default)

Ligus doesn't use these routes and never sends a GitHub (or any forge)
token to a relay. From relay v0.3 they are off unless the operator turns them
on. Directory, account and friends routes identify the caller by a GitHub token
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
| `LANGSTROTH_RELAY_TUNNEL` | `off` (default; `/v1/tunnel/*` answers 404), `open` (any host may register), `allowlist`. Use `open` only together with an access gate, so that a host's first registration needs a token; otherwise anyone can relay phone sessions through you |
| `LANGSTROTH_RELAY_TUNNEL_HOSTS` | comma-separated host ids for `allowlist` (from `ligus remote status`). `allowlist` with no hosts leaves the tunnel off |

If an access gate is configured, a host must present a token that
passes it to register (`ligus remote enable --reg-token`). The tunnel's
control connections live in one process, so the tunnel must run on **one
instance**.

APNs push (only with the tunnel on):

| Setting | Meaning |
|---|---|
| `LANGSTROTH_RELAY_APNS_KEY_FILE` or `LANGSTROTH_RELAY_APNS_KEY` | the `.p8` key, as a file or inline PEM (`\n` escapes allowed) |
| `LANGSTROTH_RELAY_APNS_KEY_ID`, `LANGSTROTH_RELAY_APNS_TEAM_ID` | Key ID and Apple Team ID |
| `LANGSTROTH_RELAY_APNS_TOPIC` | the app's bundle id (the default topic) |
| `LANGSTROTH_RELAY_APNS_TOPICS` | more bundle ids this relay may push to, comma-separated (for example a dev build); either this or `…_APNS_TOPIC` is enough |
| `LANGSTROTH_RELAY_APNS_ENV` | `auto` (default; what each phone reports), `sandbox`, `production` |

A partial configuration or a key that doesn't parse logs an error and leaves
push off; the relay still starts. APNs settings on a relay whose tunnel is
off stop it at startup: push only goes out through the tunnel. What Apple sees: a generic alert ("New activity on your host")
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
| `LANGSTROTH_RELAY_MAX_BODY_BYTES` | 4 MiB | request body (413 above) |
| `LANGSTROTH_RELAY_MAX_LIST_BYTES` | 16 MiB (`0` = unbounded) | one `GET …/keyring`, `…/candidates` or `…/presence` response, by stored bytes; at least one row is always returned |
| `LANGSTROTH_RELAY_PUSH_RATE` / `LANGSTROTH_RELAY_PUSH_BURST` | 100/s, burst 300 | writes (envelopes, keyring, presence, candidates, pairing) per client |
| `LANGSTROTH_RELAY_READ_RATE` / `LANGSTROTH_RELAY_READ_BURST` | 100/s, burst 400 | workspace reads and SSE connects per client (429 above) |
| `LANGSTROTH_RELAY_WS_WRITER_RATE` / `LANGSTROTH_RELAY_WS_WRITER_BURST` | 20/s, burst 200 | envelope appends per (workspace, writer) |
| `LANGSTROTH_RELAY_GITHUB_RATE` / `LANGSTROTH_RELAY_GITHUB_BURST` | 30/s, burst 120 | GitHub routes per client (only when they are on) |
| `LANGSTROTH_RELAY_INVITE_RATE` / `LANGSTROTH_RELAY_INVITE_BURST` | 0.05/s, burst 10 | account invites per (sender, recipient) (GitHub routes only) |
| `LANGSTROTH_RELAY_INVITE_REQUIRE_FRIEND` | off | account invites only reach existing friends (GitHub routes only) |
| `LANGSTROTH_RELAY_MAX_SSE` | 2000 | concurrent SSE streams per instance (503 above) |
| `LANGSTROTH_RELAY_MAX_SSE_PER_CLIENT` | 100 | concurrent SSE streams per client (429 above) |
| `LANGSTROTH_RELAY_FRIEND_CAP` | unlimited | friends per account (GitHub routes only) |
| pairing codes | 10 failed lookups per minute per IP, then 429 | fixed |
| tunnel | 8 sessions per host, 30 connects per IP per minute | fixed |

Envelope pages are at most 1000 envelopes or 4 MiB. The relay sends no CORS
headers. A "client" is the token subject where one applies, else the address
(see [Client addresses behind a proxy](#client-ip)).

Since v0.2.0 the relay also answers:

| Status | When |
|---|---|
| `400` | a workspace id longer than 512 bytes, a device id longer than 256 bytes, or a malformed JSON body |
| `507` | the memory store is at `LANGSTROTH_RELAY_MAX_STORE_BYTES`, or a workspace's keyring is at its rotation or byte cap |
| `413` | a body over `LANGSTROTH_RELAY_MAX_BODY_BYTES`, or a key rotation over `LANGSTROTH_RELAY_MAX_ROTATION_BYTES` |

Pairing codes take only payloads safe for the operator to read (a peer
friend code or a `sealed1:` payload; a workspace invite gets `422`) and are
single-use.

### Community limits {#community-limits}

For a relay that people you don't know can use. Every limit here is **off
by default**. The relay's `deploy/fly.toml` carries a commented profile with
values for one small VM, and the Helm chart sets them under `limits.*`.
Quotas refuse new writes (`507`) and never evict, so a full workspace stays
readable.

| Setting | Limits |
|---|---|
| `LANGSTROTH_RELAY_ROOM_MAX_BYTES` | stored bytes per workspace (envelope bodies and key rotations); `507 room_bytes` |
| `LANGSTROTH_RELAY_ROOM_MAX_ENVELOPES` | stored envelopes per workspace; `507 room_envelopes` |
| `LANGSTROTH_RELAY_MAX_TOTAL_ROOM_BYTES` | workspace bytes across the relay, on either store; `507 total_bytes` |
| `LANGSTROTH_RELAY_MAX_ROOMS_PER_CLIENT` | workspaces one client may create (token subject, else address); `429 client_rooms` |
| `LANGSTROTH_RELAY_MAX_ROOMS` | workspaces across the relay (the bound that holds when clients rotate addresses); `507 total_rooms` |
| `LANGSTROTH_RELAY_ROOM_RETENTION_DAYS` | delete a workspace's envelopes, key rotations and device blobs once nothing has been appended for N days; its sequence counter and roster stay |
| `LANGSTROTH_RELAY_IP_RATE` / `…_BURST` | every request per client except `/`, `/v1/health` and `/metrics` (burst defaults to twice the rate); `429 ip_rate` |
| `LANGSTROTH_RELAY_ROOM_RATE` / `…_BURST` | every request to one workspace, all clients together; `429 room_rate` |
| `LANGSTROTH_RELAY_TUNNEL_PUSH_RATE` / `…_BURST` / `…_PER_DAY` | pushes per tunnel host (these three default to `1` / `5` / `2000`; `0` = no limit) |

Every `429` and `507` carries a `Retry-After` header and a JSON body with a
stable code, for example
`{"error": "room_bytes", "message": "room storage quota reached", "retryAfter": 3600}`.
A refused write stored nothing, so retrying later is safe. The relay's
`docs/reference.md` lists every code.

One relay can also serve several organisations that don't trust each other
(`LANGSTROTH_RELAY_TENANCY`); see [Multi-tenant](../deploy/multi-tenant.md).

## Monitoring

| Endpoint | Auth | Use |
|---|---|---|
| `GET /v1/health` | none | liveness **and** readiness: round-trips the store (cached 1 s); `200 ok` or `503` |
| `GET /metrics` | open, or bearer `LANGSTROTH_RELAY_METRICS_TOKEN` | Prometheus: `hive_relay_requests_total`, `hive_relay_requests_by_class`, `hive_relay_requests_in_flight`, `hive_relay_limit_rejections_total{limit}`, `hive_relay_rooms`, `hive_relay_room_bytes` (and per-tenant series with tenancy) |
| `GET /` | none | a small HTML status page |

Set `LANGSTROTH_RELAY_METRICS_TOKEN` on any relay reachable from the internet.
Logs go to stdout/stderr.

## Scaling

The relay is **single-replica by design**: pairing codes, live-push (SSE)
fan-out, rate limits and the tunnel's control connections live in the
process. Run one instance.

- **Memory or snapshot store:** one process serving every route.
- **Postgres:** the Helm chart's `split` topology (the default when a
  database is configured) runs one `relay-sync` pod for every route except
  `/v1/tunnel/*` and one `relay-tunnel` pod for the tunnel
  (`LANGSTROTH_RELAY_ROUTES=tunnel`), both on the same database. The chart
  refuses more than one sync replica. Postgres gives durability and backups,
  not horizontal scaling.

## Backups

A relay holds no data that only it has: every device keeps the full log of
the workspaces it belongs to. Losing the relay loses queued traffic for
devices that were offline longer than peers kept serving it, relay users and
tokens, membership rosters (if used) and phone tunnel registrations. Back it
up for convenience, not as your only copy.

- **Snapshot store:** copy `relay-state.json` from the data dir. It is
  replaced atomically, so a copy is always consistent.

    ```sh
    install -d -m 700 /var/backups/langstroth-relay
    cp /var/lib/langstroth-relay/relay-state.json /var/backups/langstroth-relay/relay-state-$(date +%F).json
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

## Point clients at it {#point-clients}

Give the relay an `https://` URL (the base origin, without `/v1`), then:

- **Desktop:** when you create a workspace with no relay set, pick **Your
  own relay** and enter the URL; or set **Settings → Team sync → Relay URL**
  on every device that should sync (and **Relay access token** for a gated
  relay). Check it with `curl -s https://relay.example.com/v1/health`.
- **CLI, `ligus serve`, `ligus worker`:** `ligus enroll --file <invite-file> [--token @<token-file>]`
  saves the relay URL from the invite; `LIGUS_RELAY_URL` and
  `LIGUS_RELAY_ACCESS_TOKEN` override it.
- **Everyone else** gets the relay URL inside the workspace invite: only
  the person who creates a workspace picks a relay. See
  [The community relay](community-relay.md#the-url-travels-in-the-invite).

## Checklist for an internet-facing relay

- [ ] TLS in front; `LANGSTROTH_RELAY_TRUSTED_PROXIES` set to your proxy's addresses
- [ ] `DATABASE_URL` or `LANGSTROTH_RELAY_DATA_DIR` set; `LANGSTROTH_RELAY_REQUIRE_DB=1` with Postgres
- [ ] `LANGSTROTH_RELAY_MAX_STORE_BYTES` set on a memory or snapshot store
- [ ] no `LANGSTROTH_RELAY_MAX_ENVELOPES` or `LANGSTROTH_RELAY_RETENTION_DAYS` (the relay won't start); [community limits](#community-limits) if strangers can reach it
- [ ] an access gate (`LANGSTROTH_RELAY_TOKEN_PUBKEY` or `LANGSTROTH_RELAY_ACCESS_TOKENS`) unless you want a public relay, verified with an unauthenticated request
- [ ] `LANGSTROTH_RELAY_WORKSPACE_SCOPE=claims` (or `strict`) with workspace-scoped signed tokens
- [ ] issuer private key kept off the relay host
- [ ] `LANGSTROTH_RELAY_METRICS_TOKEN` set
- [ ] tunnel `off` unless you use the phone app; `allowlist` if only your boxes should register, `open` only with an access gate
- [ ] backups scheduled

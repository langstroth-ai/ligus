# Ligus Relay API

This document defines the client-side relay contract used by `RelayTransport`.

The relay is intentionally a dumb, ciphertext-only coordination service:
- it stores opaque encrypted envelopes
- it stores per-version sealed workspace-key snapshots
- it stores ephemeral presence slots whose contents it can't read
- it never needs plaintext session payloads

## Base URL

Relay clients are configured with a single endpoint, for example:

```toml
[transport]
kind = "relay"

[transport.relay]
endpoint = "wss://relay.ligus.example/v1"
account_token_env = "LIGUS_RELAY_TOKEN"
```

`RelayTransport` talks to the relay over **REST** at `https://.../v1`.
Clients discover new envelopes with `GET .../envelopes?after=`, and can hold
`GET .../events` (server-sent events) open to be woken when one is appended.

## Auth

The relay uses **two independent** credentials, on **separate headers**, so a
token-gated relay and a directory identity never collide:

```http
Authorization: Bearer <relay-access-token>      # access: may I connect?
X-Hive-Github-Token: <github-oauth-token>        # identity: who am I (directory)?
```

### Access token (`Authorization: Bearer`)

Controls *whether a client may use the relay at all*. The server's policy comes
from the `LANGSTROTH_RELAY_ACCESS_TOKENS` env var:

- **unset / empty ⇒ `Open`** — every request is admitted (self-host default).
- **comma-separated list ⇒ `Tokens`** — only requests whose bearer is in the set
  are admitted; everything except `GET /v1/health` is behind a
  `require_entitlement` middleware that returns `401`/`403` otherwise.

The desktop client sends this from `relay_access_token` (Settings → Team sync, or
`LIGUS_RELAY_ACCESS_TOKEN`); `RelayClient::with_auth(token)` attaches it to every
request. It is intentionally transport-agnostic — an operator (or langstroth-auth)
issues and revokes tokens without changing the relay surface.

The relay's `EntitlementPolicy` resolves from the environment, most specific
first:

1. **`LANGSTROTH_RELAY_TOKEN_PUBKEY`** (hex/base64 Ed25519 public key) ⇒ **signed
   tokens**. The bearer is a compact `hrt1.<b64url(claims)>.<b64url(sig)>` minted
   by an issuer holding the matching private key; the relay *verifies but never
   mints*. Claims carry `sub`, `exp`, optional `workspaces`, RBAC `caps`, and the
   informational `plan`, `max_members`, `retention_days` and `turn`. Verified
   claims are passed to the guard seams. See `token.go` in the relay and
   [Self-hosting a relay](../docs-site/docs/operate/networking/self-host.md#access-control).
2. **`LANGSTROTH_RELAY_ACCESS_TOKENS`** (comma-separated) ⇒ a static **allowlist**
   (coarse on/off).
3. otherwise ⇒ **open** (self-host default).

### Identity (`X-Hive-Github-Token`)

Used **only** by the directory endpoints (register / lookup), where the relay
calls GitHub `/user` to bind a request to a GitHub account. It rides its own
header so it doesn't consume `Authorization` (which carries the access token). For
backward compatibility the relay falls back to the bearer if the header is
absent.

## Admin APIs (user/token management)

`/v1/admin/*` manages durable relay access users + tokens. These routes bypass
the access gate and are instead authorized by an **admin authorizer** seam
(the reference relay leaves it disabled → `404`; a build that supplies one enables it,
e.g. gated on a GitHub-admin allowlist authenticated by `x-hive-github-token`).
Tokens are stored only as SHA-256 hashes; the raw value is returned once at
issue time.

| Method + path | Body | Returns |
|---|---|---|
| `POST /v1/admin/users` | `{"name","login?","label?"}` | `{user, token, raw}` — `raw` shown once |
| `GET /v1/admin/users` | — | `[{…user, tokens:[…]}]` (no hashes) |
| `POST /v1/admin/users/{id}/tokens` | `{"label?"}` | `{user, token, raw}` |
| `POST /v1/admin/users/{id}/disabled` | `{"disabled": bool}` | `{"ok":true}` |
| `DELETE /v1/admin/tokens/{id}` | — | `204` |

A token issued here resolves to access claims (`sub` = the user, `plan` =
`team`); revoking a token or disabling its user takes effect immediately.

## Envelope APIs

### POST `/v1/workspaces/{workspaceID}/envelopes`

Appends one or more sealed envelopes.

Request body:

```json
{
  "envelopes": [
    {
      "workspaceID": "UUID",
      "sequence": 42,
      "signerDeviceID": "UUID",
      "signature": "base64",
      "keyVersion": 3,
      "nonce": "base64",
      "ciphertext": "base64"
    }
  ]
}
```

Response:
- `204 No Content` or `200 OK`

### GET `/v1/workspaces/{workspaceID}/envelopes?after={sequence}&limit={limit}`

Returns sealed envelopes after the provided sequence. This poll is the **only**
mechanism for discovering new envelopes — clients call it on an interval with
the last sequence they've seen.

Response body:

```json
{
  "envelopes": [
    {
      "workspaceID": "UUID",
      "sequence": 42,
      "signerDeviceID": "UUID",
      "signature": "base64",
      "keyVersion": 3,
      "nonce": "base64",
      "ciphertext": "base64"
    }
  ]
}
```

Servers may also return a bare array of envelopes; the current client accepts both forms.

## Workspace-key rotation APIs

These endpoints exist so a second client can import its sealed workspace-key material for a specific `keyVersion` before attempting to decrypt a relayed envelope tagged with that version.

### POST `/v1/workspaces/{workspaceID}/keyring`

Publishes a `WorkspaceKeyRotationSnapshot` to the workspace's keyring.

Request body:

```json
{
  "workspaceID": "UUID",
  "keyVersion": 3,
  "sealedKeysByDeviceID": {
    "DEVICE_UUID": {
      "keyVersion": 3,
      "createdAt": "2026-05-27T23:00:00Z",
      "ciphersuite": "Curve25519_SHA256_ChachaPoly",
      "encapsulatedKey": "base64",
      "ciphertext": "base64",
      "recipientPublicKey": "base64"
    }
  },
  "rotatedAt": "2026-05-27T23:00:00Z"
}
```

Response:
- `204 No Content` or `200 OK`

### GET `/v1/workspaces/{workspaceID}/keyring`

Returns **all** sealed workspace-key snapshots published to the workspace's
keyring. Clients pick the newest version they can decrypt for their device;
there is no per-version subpath.

Response:
- `200 OK` with an array of `WorkspaceKeyRotationSnapshot`

## Presence APIs

Presence is a set of small, overwritten-in-place slots per workspace, never
part of the event log. The relay stores each slot's `data` as an opaque blob:
every current Ligus client seals it (ChaCha20-Poly1305 under the workspace
key, the same `SealedEnvelope` as an event body), so the relay never sees who
is online, which chat they are in, whether they are typing or which device
holds a lease.

### POST `/v1/workspaces/{workspaceID}/presence`

Upserts one slot.

Request body:

```json
{ "device_id": "human:DEVICE_UUID", "data": { "human": { "...": "SealedEnvelope" } } }
```

- `device_id` is the slot name (at most 256 bytes). Ligus uses three kinds:
  `human:<device id>` (a person online, and typing in a chat),
  `host:<host id>` (a host's liveness beat) and `lease:<scope>:<scope id>`
  (a coordinator lease's renewal beat). The slot name is visible to the
  relay; the ids in it are ones the relay already sees in other requests.
- `data` is any JSON value; the relay doesn't parse it. Ligus sends one of
  `{"human": SealedEnvelope}`, `{"hostBeat": SealedEnvelope}` or
  `{"leaseBeat": SealedEnvelope}`. Readers ignore a plaintext blob (an older
  client), so a relay can't inject a typing indicator or a beat.
- A write replaces the slot. A workspace holds at most
  `LANGSTROTH_RELAY_MAX_DEVICE_BLOBS_PER_WS` slots (default 1,000): a write
  for a new slot past the cap evicts another. Writes count against the
  store's byte budget and the write rate limit.
- In a workspace with a member roster, any member (Viewer included) may
  write, as for envelopes; only key rotations need Contributor.
- There is no TTL on the relay: a slot stays until it is overwritten or the
  workspace is deleted. Readers judge staleness from the sealed timestamp
  (a person's beat counts for 8 seconds, a host is online for 90).
- Writing presence doesn't wake `GET .../events` streams; only a new
  envelope does.

Response: `200 OK` with `{"ok": true}`.

### GET `/v1/workspaces/{workspaceID}/presence`

Returns every slot as an object, slot name to blob:

```json
{
  "human:DEVICE_UUID": { "human": { "...": "SealedEnvelope" } },
  "host:HOST_UUID": { "hostBeat": { "...": "SealedEnvelope" } }
}
```

The response is bounded by `LANGSTROTH_RELAY_MAX_LIST_BYTES` (default
16 MiB of stored blob bytes; at least one slot is always returned). Readers
open each blob with the workspace key at the authorized head and drop blobs
under an older key, blobs whose slot doesn't match the device or host they
name, and blobs stamped too far ahead or too long ago. A person's beat is
unsigned (a member could fake another member's typing indicator, which grants
nothing); a lease beat is also signed by the holder's device key.

## Invariants

- The relay must not require or inspect plaintext `SessionEvent` payloads.
- `keyVersion` on `SealedSessionEventEnvelope` is authoritative for decryption.
- Clients are responsible for signature verification after decrypting the inner envelope.
- Relay storage may be self-hosted; nothing in this contract assumes an Apple-only client.

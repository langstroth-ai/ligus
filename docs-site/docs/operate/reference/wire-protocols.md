# Wire protocols

Ligus's state is an append-only log of **signed event envelopes**. The same
envelope is the unit of local storage, of relay sync, and (eventually) of
direct peer transfer — so this one shape is the contract an independent
implementation would need.

## 1. Event envelope + local store

State lives in **SQLite** (`hive.db` in the app data dir): an append-only
`events` table, one signed envelope per row (`envelope_json` TEXT), projected
into a `ChatSession` on read. Each envelope serializes to JSON with `camelCase`
keys:

```json
{
  "id": "uuid",
  "eventId": "uuid",
  "sessionId": "uuid",
  "workspaceId": "uuid",
  "sequence": 42,
  "timestamp": "ISO8601",
  "actorStamp": { "…": "…" },
  "payload": { "kind": "messageAppended", "…": "…" },
  "scope": "session",
  "signerDeviceId": "uuid",
  "signature": "base64-Ed25519"
}
```

`payload.kind` is the tagged event variant — e.g. `messageAppended`,
`messageChunkReceived`, `messageCompleted`, `sessionTitleChanged`,
`agentRosterUpdated`, `proposalUpserted`, `messageReactionAdded`. The projector
folds them in order; a `sessionSnapshot` seeds/bounds replay.

[Proposal sets](../../ligus/features/proposal-sets.md) (2.0) add `proposalSetOpened`,
`proposalSetAlternativeAdded`, `scoutAssessmentPosted`,
`proposalSetRoundAdvanced`, `proposalSetQuorumReached`,
`proposalSetAlternativeStopped`, `proposalSetTuned`,
`proposalSetPreflightRecorded`, `proposalSetDecided` and `proposalReverted`.
Proposals in a set carry optional `setId` and `taskClass` fields. A peer
that doesn't know a kind folds it as a no-op and keeps the event, so new kinds
are backward compatible.

The signature is **Ed25519** over a canonical preimage (sorted-key JSON,
ISO8601 dates) of the envelope's identifying fields + payload. See
`crates/hive-core/src/{crypto,events}.rs`. Envelopes that fail verification are
quarantined, not applied.

## 2. Relay sync (HTTP/JSON)

The relay ([ligus-ai/relay](https://github.com/ligus-ai/relay)) is a per-workspace forwarding mailbox keyed by
a **room id**. Devices on the same relay URL + room converge.

- `GET  /v1/health` → `ok`
- `POST /v1/workspaces/<room>/envelopes` → append signed envelopes to the room
- `GET  /v1/workspaces/<room>/envelopes?after=<sequence>&limit=<n>` → one
  page of envelopes newer than `<sequence>`. A relay that has more marks the
  page with `X-Hive-More: true` and the cursor to continue from in
  `X-Hive-Next-After`; clients ask for pages of 500 and keep fetching until
  caught up. A relay without paging returns everything and neither header.

Clients push their unsynced envelopes and pull new ones on a short interval,
deduping by `eventId` (so restarts / double-sends are harmless). A room
carries only its own workspace: a client pushes only events stamped with
that room's workspace id and drops any event stamped with another. When a
**workspace key** is set, each envelope body is sealed with ChaCha20-Poly1305
*before* upload — the relay only ever stores ciphertext. See
[Self-hosting a relay](../networking/self-host.md).

## 3. MCP (subprocess or HTTP)

Standard [Model Context Protocol](https://modelcontextprotocol.io). Ligus
implements `initialize`, `tools/list`, and `tools/call`, over two transports:

- **stdio** — Ligus spawns the binary; JSON-RPC over stdin/stdout.
- **http** — JSON-RPC POSTs to the configured endpoint.

An installed server stays inert until you enable it. Wire types and bridges
live in `crates/hive-runtime/src/mcp/`.

## Canonical JSON conventions

Every signed payload uses **sorted keys**, **no escaped slashes**, and
**ISO8601** dates. This is the wire contract: an independent implementation MUST
produce byte-identical preimages or signatures won't match. A stability test
guards against accidental drift.

## Roadmap (not in the current build)

The relay also exposes `/v1/workspaces/<room>/{candidates,presence}`. The
presence slots carry host liveness: each running host publishes a beat about
every 30 seconds, sealed under the workspace key, and a host counts as online
while its last beat is under 90 seconds old. The following are tracked
follow-ups and **not wired** in the current Rust build — relay forwarding
(above) is the multiuser path today:

- **Direct peer-to-peer** — STUN / UDP hole-punch / TURN fallback and a
  length-prefixed peer-link frame protocol.
- **LAN discovery** — zero-config local peer announcement.
- **Collaborative-text CRDT** — concurrent editing of proposal bodies.

## Legacy protocol identifiers

The strings below are legacy identifiers from the project's former name, Hive.
They are protocol and cryptographic constants, not branding, so they keep the
`hive` spelling: renaming any of them would break mixed-version peers, paired
phones, relays, invites, or (for the cryptographic ones) every existing
signature and encrypted workspace. Tests pin their exact bytes. If one ever has
to change, it ships as a new protocol version accepted alongside the old one.
The phone deep-link scheme is the exception that grows: phones accept both
`ligus://` and `hive://`, and `ligus serve` keeps emitting `hive://` until
every paired phone runs a build that registers `ligus://`.

| Identifier | Kind | Where |
|---|---|---|
| `hive-envelope-v3\0` | Signature context (event envelopes) | `hive-core/src/crypto.rs` |
| `hive-device-cert-v1\0` | Signature context (device certificates) | `hive-core/src/crypto.rs` |
| `hive-workspace-key-seal-v1` | HKDF info (sealing workspace keys to devices) | `hive-core/src/e2ee.rs` |
| `hive-workspace-key-argon2id` | Argon2id salt (passphrase workspace keys) | `hive-core/src/e2ee.rs` |
| `hive-device-enroll-v1\0` | Signature context (device enrollment codes) | `hive-runtime/src/device_enroll.rs` |
| `hive-tunnel/1` | HKDF salt and `ctx` label (phone tunnel) | `hive-cli/src/serve/tunnel/mod.rs` |
| `hive-tunnel.1` | WebSocket subprotocol (phone tunnel) | `hive-cli/src/serve/tunnel/mod.rs` |
| `hive/sync/1` | iroh ALPN (peer sync) | `hive-runtime/src/peer_iroh.rs` |
| `hrt1.` | Relay access-token format | relay, clients |
| `hpc1.` | Phone credential format | `hive-cli/src/serve/tunnel/cred.rs` |
| `hivews1:` | Workspace invite prefix | app, CLI |
| `hivedev1:` | Device enrollment code prefix | `hive-runtime/src/device_enroll.rs` |
| `hive_`, `hivepeer1:` | Peer code prefixes | `hive-runtime/src/peer.rs` |
| `X-Hive-Github-Token`, `X-Hive-More`, `X-Hive-Next-After`, `X-Hive-Event-Id` | Relay HTTP headers | relay, clients |
| `hive://` | Phone deep-link scheme (`hive://pair`, `hive://chat/<id>`) | `ligus serve`, phone app |

Strings added in 2.0 use the `ligus` spelling. Among them, the SSHSIG
namespace of a [linked account](../../ligus/features/linked-identities.md),
`ligus-identity-link-v1@ligus.ai` (`hive-core/src/identity_link.rs`, pinned by
`link_namespace_is_frozen`), is a length-prefixed SSHSIG field passed to
`ssh-keygen -Y sign -n`, so unlike the signature contexts above it has no
`\0` terminator. See the [statement reference](identity-link-statement.md).

## Versioning

Protocols carry an explicit version: the relay namespaces its routes under
`/v1/…` (a breaking change bumps to `/v2/…` and both coexist during the upgrade
window), and the envelope's `scope` defaults to `session` when missing, so new
fields can be added without breaking old logs.

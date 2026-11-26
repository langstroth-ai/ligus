# Code-match approval: wire format (implementation note)

Status: implementation contract for `docs/design/code-match-approval.md`
(Ligus 0.4). That note says what and why; this one pins the bytes every
client and the relay must agree on. When the two disagree on a byte, this
note wins; on anything else, the design note does. Ligus is short for
*ligustica*, the Italian honeybee.

Pure functions live in `crates/ligus-core/src/match_code.rs` with golden
vectors in `crates/ligus-core/tests/golden/match_code_v1.jsonl` (computed
independently with Python's standard library). The phone apps copy that file
byte for byte and must pass it.

Conventions: `b64u` is base64url without padding. `hex` is lowercase hex.
`ms` is Unix milliseconds. UUIDs are the 16 raw bytes in hashes and the
hyphenated string in JSON. `put(x)` is `u32le(len(x)) ‖ x`.

## 1. Keys every member derives (one per key epoch)

```
ikm         = the 32-byte workspace key of epoch E
authSeed    = HKDF-SHA256(salt = "ligus-approval-mailbox-v1", ikm, info = "auth")[..32]   Ed25519 seed
sealSeed    = HKDF-SHA256(salt, ikm, info = "seal")[..32]                                X25519 seed
mailbox     = SHA256(authPub)[..16]                                                       hex in URLs (32 chars)
putCap(n)   = HKDF-SHA256(salt, ikm, info = "put" ‖ u32le(n))[..16]                       n = the relay's counter
```

`ligus_core::match_code::{MailboxKeys, put_cap, mailbox_id}`.

## 2. The approval address

```
ligusaddr1:<b64u(JSON)>
{"v":1,"relay":"https://relay.example","workspaceId":"<uuid>","founderPin":"<32 hex, FounderPin::fingerprint_hex>",
 "epoch":3,"mailbox":"<32 hex>","sealKey":"<b64u 32>","putCap":"<b64u 16>"}
```

Not a secret. A requester refuses an address with `v != 1`, a non-`https`
relay (unless the build allows insecure relays for tests), or keys of the
wrong length.

## 3. Messages

### 3.1 Sealed blob on the relay

`blob = b64u(ephemeralPublic(32) ‖ ciphertext)` where the pair is
`ligus_core::e2ee::seal(recipientX25519, plaintext)`. Recipients try to open
every blob; failing to open means "not for me". A posted message also carries
a plaintext routing hint `to`: `members` (sealed to `sealKey`), `requester`
(to `requesterKa`) or `approver` (to `approverKa`). The hint is never trusted.

### 3.2 Signed envelope (the plaintext inside a seal)

```
{"t":"open","alg":"ed25519"|"p256","key":"<b64u signer public key>","body":"<a JSON object, as a string>","sig":"<b64u>"}
```

`sig` is over `match_code::message_preimage(t, body)` =
`ctx(t) ‖ utf8(body)`, with `ctx` one of `ligus-match-{request,claim,open,reveal,offer,ack,result,abort}-v1\0`.
Ed25519 keys are 32 bytes; P-256 keys are the 65-byte SEC1 uncompressed point
and signatures the 64-byte low-S `r ‖ s` (`ligus_core::crypto::verify_p256`).
The verifier verifies over the exact `body` string, then parses it. Unknown
body fields are ignored; missing required ones refuse the message.

### 3.3 Bodies

Every body carries `"rid":"<32 hex>"` (16 random bytes) and the verifier
refuses a body whose `rid` isn't the one it is reading.

| t | Sealed to (`to`) | Signed by | Body fields |
|---|---|---|---|
| `request` | `sealKey` (`members`) | the device key inside `code` (box, computer: Ed25519; phone: P-256) | `v:1`, `kind:"box"\|"computer"\|"phone"`, `rid`, `mailbox` (32 hex), `code` (the unchanged `ligusjoin1:`/`liguslink1:`/`ligusphone1:` text), `requesterKa` (b64u 32), `readKey` (b64u 16), `commit` (b64u 32), `createdAt` (ms) |
| `claim` | `sealKey` (`members`) | approver device key | `rid`, `approverDeviceId`, `approverName` (a display hint), `at` |
| `open` | `requesterKa` (`requester`) | approver device key (`key` = `approverSigKey`) | `rid`, `approverDeviceId`, `approverSigKey` (b64u), `approverKa` (b64u 32, ephemeral per exchange), `approverPerson:{accountId,name}`, `nA` (b64u 32), `at` |
| `reveal` | `approverKa` (`approver`) | requester device key | `rid`, `nR` (b64u 32) |
| `offer` | `requesterKa` (`requester`) | `approverSigKey` from the `open` | computers only: `rid`, `grantDigest` (hex SHA-256 of the sealed `ligusgrant1:` text, built and sealed now and kept in memory), `person:{accountId,name}` (equal to the `open`'s `approverPerson`), `approverDeviceId` (equal to the `open`'s) |
| `ack` | `approverKa` (`approver`) | requester device key | computers only: `rid`, `grantDigest` (the offer's) |
| `result` | `requesterKa` (`requester`) | `approverSigKey` from the `open` | `rid`, `outcome:"approved"\|"mismatch"\|"declined"\|"expired"`, `grant` (the `ligusadmit1:`/`ligusgrant1:` text, or null), `grantDigest` (hex SHA-256 of the grant's UTF-8 bytes, or null) |
| `abort` | posted twice: `sealKey` (`members`) and `requesterKa` (`requester`); the requester posts to `members` and, after an `open`, `approver` | anyone (its own key) | `rid`, `reason:"mismatch"\|"cancelled"\|"contested"` |

Request checks on the approver: the `key` is the key the `code` carries
(`JoinRequest.device_public_key`, `LinkRequest.device_public_key`,
`PhoneCode.public_key`), `alg` matches it, the code decodes and verifies as it
does today, `kind` matches the code's prefix, `mailbox` is the mailbox it was
read from, `requesterKa` is the code's key-agreement key for a box or a
computer (a phone's is a fresh X25519 key), and `createdAt` is within 15
minutes (5 minutes of clock skew ahead allowed).

`kind:"box"` with `code` a `ligusjoin1:` is the only way a box request is
signed: the `ligusjoin1` codec itself is unchanged.

**Computers: certify on acknowledgement** (`identity-model-v2.md` 8.1).
After the codes match the approver builds and seals the grant, keeps it in
memory and sends `offer`. The requester checks it (signed by the `open`'s
`approverSigKey`, the same person and device) and, once its person confirmed
the code, sends `ack`. Only after a valid `ack` (signed by the request's
device key, over the offered digest) does the approver run the passkey
ceremony and publish `DeviceCertificateAdded`, drafted fresh, and then send
`result{approved, grant, grantDigest}`; the requester refuses a grant whose
digest isn't the acknowledged one. No valid `ack` within **2 minutes** of the
`offer`: nothing is signed or published, no key leaves the approver ("Linking
<label> didn't finish. Nothing was added."). A key rotation before the
publish aborts the exchange. A computer exchange uses at most claim, open,
reveal, offer, ack and result: within the 10 messages per request. Boxes and
phones have no `offer` or `ack`. A phone's `code` is carried verbatim, so a
later additive field inside `ligusphone1:` rides through unchanged.

### 3.4 The code

`match_code::{commit, Transcript::digest, code40, encode}`, design note 6.1.
`approverDeviceId`, `approverSigKey`, `approverKa` and `nA` come from the
`open`; `requesterKa` and `commit` from the request; `request_code` is the
request body's `code` string. The requester checks
`commit == SHA256("ligus-match-commit-v1\0" ‖ rid ‖ nR)` is its own (it is), and
the approver checks it when the `reveal` arrives and aborts on a mismatch.

## 4. Relay routes (`langstroth-relay`, additive)

All bodies are JSON. Errors are problem documents carrying the same code in
both `error` and `code` (`{"error":"request_unknown","code":"request_unknown","message":…}`);
`429` and `507` also carry `Retry-After`. Every route answers a plain 404
(no problem document) when mailboxes are off. Clients read a 404 this way:

- `mailbox_unknown`: no mailbox at that id. The relay keeps mailboxes in
  memory and forgets them on a restart, so a member re-registers
  (`ensure_registered`, counter 0) when it sees this.
- `request_unknown`: the `rid` is unknown, expired or deleted: the request
  is dead.
- anything else, and any 404 from `PUT /v1/mailboxes/{mailbox}`: "this relay
  has no mailbox routes" (fall back to paste, exit 3).

Other problem codes: `rid_exists`, `stale_counter`, `duplicate_message` (409); `bad_put_cap`,
`bad_read_key`, `bad_member_auth`, `auth_required`, `token_required`,
`bad_signature` (401/403); `mailbox_mismatch`, `invalid_request` (400);
`too_large` (413); `mailbox_full`, `too_many_messages`, `mailbox_register`,
`mailbox_polls` and the rate codes (429); `mailboxes_full`,
`mailbox_storage` (507). Blobs are non-empty base64url, times are Unix ms,
and a request's message `seq` starts at 1.

### 4.1 Auth

- **Member**: header `X-Ligus-Mailbox-Auth: <ts ms>.<b64u Ed25519 sig>` by
  the mailbox auth key over `match_code::member_preimage(method, path, ts,
  body)` = `"ligus-mailbox-member-v1\0" ‖ method ‖ 0x00 ‖ path ‖ 0x00 ‖
  i64le(ts) ‖ SHA256(body)`; `path` is the request path without the query
  string; `body` is empty for GET and DELETE. The relay verifies it with the
  registered `authPub` and refuses a `ts` more than 300 s from its clock. On
  a token-gated relay a member also sends its normal
  `Authorization: Bearer hrt1…` access token, checked as any other request
  (any valid token; mailboxes aren't bound to rooms). Register and reset
  send it too.
- **Requester**: `X-Ligus-Read-Key: <b64u 16>` whose SHA-256 equals the
  `readKeyHash` stored with that `rid`. A requester needs no token.
- **Put**: `X-Ligus-Put-Cap: <b64u 16>` whose SHA-256 equals the mailbox's
  current `putCapHash`. With `mailboxes: token` the put also needs a valid
  access token.

### 4.2 Routes

```
GET    /v1/mailboxes/{mailbox}
       no auth -> 200 {"mailbox","counter"} | 404 {code:"mailbox_unknown"}

PUT    /v1/mailboxes/{mailbox}
       {"authPub":"<b64u 32>","putCapHash":"<b64u 32>","counter":N,"sig":"<b64u 64>"}
       sig: Ed25519 by authPub over match_code::register_preimage(mailbox, putCapHash, counter)
       relay checks mailbox == hex(SHA256(authPub)[..16]) and the sig.
       new mailbox -> 201; same counter and same hash -> 200 (idempotent);
       exactly counter + 1 -> 200 (reset: the old cap stops working); anything else
       (lower, higher by more than one, or the same counter with another hash)
       -> 409 {code:"stale_counter","counter":<the current counter>}
       mailbox ≠ the authPub's id -> 400 {code:"mailbox_mismatch"}
       -> {"mailbox","counter"}

PUT    /v1/mailboxes/{mailbox}/requests/{rid}
       X-Ligus-Put-Cap; {"blob":"<= 8 KiB","readKeyHash":"<b64u 32>"}
       -> 201 {"rid","putAt","expiresAt"} | 403 bad_put_cap | 404 mailbox_unknown | 409 rid_exists
          | 429 rate (10 puts/min/client address) | 429 {code:"mailbox_full"} (64 live)

GET    /v1/mailboxes/{mailbox}/requests                       member
       -> 200 {"requests":[{"rid","blob","putAt","expiresAt","msgCount"}]}   (live ones only, oldest first)

POST   /v1/mailboxes/{mailbox}/requests/{rid}/msgs            member or requester (read key)
       {"blob":"<= 32 KiB","to":"members"|"requester"|"approver"}
       -> 201 {"seq":N} | 404 request_unknown (unknown or expired rid) | 409 {code:"duplicate_message"} (the same
          blob already on this rid) | 429 {code:"too_many_messages"} (10 per rid)

GET    /v1/mailboxes/{mailbox}/requests/{rid}/msgs?after=N&wait=S   member or requester
       long poll up to min(S, 25) s while nothing is after N
       -> 200 {"msgs":[{"seq","blob","to","from":"member"|"requester","at"}]}

DELETE /v1/mailboxes/{mailbox}/requests/{rid}                 requester (read key)
       -> 204; the request and its messages are gone | 404 request_unknown
```

`from` is what the relay saw (member auth or a read key), a hint only.
Requests and their messages expire 15 minutes after the put. A `rid` is never
overwritten, and an expired or deleted `rid` can't be put again (the relay
keeps its hash until the expiry would have passed).

**Who holds the read key.** `readKey` travels inside the request, sealed to
`sealKey`, so every member that holds the epoch key (and a paired phone,
through its box's `GET /v1/approvals`) learns it and can read the request's
messages, post as the requester, or delete the request. Accepted: what it
reads is sealed to the requester or the approver, anything it posts as the
requester fails their signature checks, and deleting a request is denial of
service only, like a racing `open` (design note threat T7).

## 5. The serve API for a phone approver (`ligus serve`, paired phones only)

The phone is the approver (design note 5.2): it makes `nA` and its ephemeral
key, signs with its P-256 key, seals every message it sends itself, and the
box only carries bytes. Routes are under the existing phone-authenticated
tunnel API and answer 404 on an older box (the phone keeps today's flows).

```
GET  /v1/approvals
     -> {"approvals":[{"rid","workspaceId","workspaceName","kind","label","mailbox","epoch",
                       "sealKey":"<b64u>","putAt","expiresAt",
                       "request":"<the request's plaintext envelope, as a string>",
                       "claims":[{"approverDeviceId","approverName","at","verified":bool}]}]}
GET  /v1/approvals/{rid}/msgs?after=N
     -> {"msgs":[{"seq","to","from","at","blob","plain":"<envelope>"|null}]}
        plain: the box opens what is sealed to sealKey (claims, aborts) for the phone,
        which can't derive the seal key; everything else is passed through sealed.
POST /v1/approvals/{rid}/claim|open|result|abort
     {"blob","to"} -> 201 {"seq"} : posted to the relay with the box's member auth
```

The box admission itself uses the existing `/v1/admissions/prepare`,
`presence` and `commit`; `commit` returns the `ligusadmit1:` code, which the
phone puts in its signed `result` (with `grantDigest`). The phone checks the
`prepare` preimage against its own parse of the request's `ligusjoin1` text,
never against the box's preview.

Push kind `approval.request`, sealed per phone like every push, payload
`{"workspaceId","rid","label","requestKind"}` (`requestKind` is box,
computer or phone; the push envelope's own `kind` is `approval.request`). Only
unclaimed box requests are pushed; device requests are listed in
`GET /v1/approvals` without a code, which neither a phone nor a box can compute
(the `open` and `reveal` are sealed to the requester and the approver). The
box stops pushing after 3 in 10 minutes per workspace and the phone's Inbox
shows "Several join requests". `/v1/admissions/commit` answers the sealed
`ligusadmit1:` text in `code` for a request-bound admission; the phone signs
`result` with its `grantDigest`.

## 6. Requester state (data dir, owner-only)

`approval-pending.json`: `[{"rid","mailbox","relay","kind","nR","readKey","createdAt","addressEpoch","workspaceId","founderPin","openSeen":bool}]`,
written with `private_file::write_private_atomic`, deleted entry by entry
once settled. Nothing here is a workspace secret; `nR` is only useful before
the reveal.

`approval-dead.json`: `[{"rid","at"}]`, the requests that ended without an
approval in the last 15 minutes (design note 6.4: the requester stops at 3).

`approval-seen.json` (an approver): `{"<mailbox hex>":{"<rid hex>":<ms>}}`,
the `rid`s this device opened, kept 24 hours.

All three are device-local, owner-only, and read by nothing else.

## 7. Paste with a live request (design note 6.6)

The paste text is unchanged (`ligusjoin1:`/`liguslink1:`), so an older
desktop accepts it. When a 0.4 approver is given pasted text and its mailbox
holds a live request whose `code` is byte-equal (after trimming whitespace),
it runs the code-match exchange for that request instead of the fingerprint
compare. Otherwise it shows the fingerprint compare exactly as today.

# Add a phone by scanning the desktop's QR (reverse-QR enrollment)

Status: **design, not built.** Reviewed in
[phone-enroll-reverse-qr.review.md](phone-enroll-reverse-qr.review.md).
Ligus is short for *ligustica*; the platform is Langstroth.

Citations are `file:line` into this repo as of `origin/main` at `b635b65f9`,
`relay/…` into `langstroth-ai/langstroth-relay` at `a5cec7b`, and `ios/…`
into the (private) iOS app repository at `38f18bb`. When this document and
the code disagree, the code wins.

---

## 1. Problem

"Add this phone" today (`docs/ios-app.md`, "Add this phone: implementation
spec"):

1. The phone shows its `ligusphone1:` code (Settings > This phone) as a QR
   code and a Copy button (`ios/Ligus/Views/PhoneCodeView.swift:25-38`). Copy
   is `.localOnly` with a 10-minute expiry, so it never reaches the Mac
   through Universal Clipboard.
2. The desktop's **Add this phone** panel
   (`web/src/components/settings/AddPhonePanel.tsx`) takes the code only as
   **pasted text**. It decodes it (`PhoneCode::decode`,
   `crates/ligus-runtime/src/phone_enroll.rs:150`), shows the fingerprint,
   and certifies with the person's passkey (`certify_phone_code`,
   `crates/ligus-runtime/src/chat_service/phone_sign.rs:350`).

Nothing on the desktop reads the phone's QR, and the copy can't leave the
phone, so the person has no way to move the code. The panel's hint ("scan
its QR code with the phone's camera app and paste here via the clipboard",
`AddPhonePanel.tsx:6-8`) describes something that doesn't work.

The usual fix: **the desktop shows a QR code, the phone scans it with the
app's own scanner, and the phone's code comes back to the desktop by
itself.** The desktop then shows "*Ana's iPhone* wants to be added", the
fingerprint (also on the phone), and asks for the passkey. Paste stays as
a fallback.

## 2. Decisions at a glance

| # | Question | Decision |
|---|---|---|
| D1 | Return path, phone → desktop | **Through the workspace's `ligus serve` box** (option a). The phone posts a request to a new box route; the box relays it to the desktop on the relay's **presence channel**, sealed under the workspace key and signed by the box's device key, the same way phone-revocation reports travel today (`crates/ligus-runtime/src/revocation_status.rs`). No relay change, no new event kind on the log. |
| D2 | A phone that isn't paired yet (the common case: pairing needs a certificate first) | A new, additive **provisional enrollment** in the tunnel (`relay-tunnel.md` §7.2 "E3b"): a phone that proves the box's pairing secret and its own device key, but isn't certified yet, gets a restricted channel to the enroll routes only. Over the direct path (bearer token) the routes are available as they are. |
| D3 | No box at all | Paste (unchanged), plus `ligus phones certify`. Owner question O1: let the phone's code (public data) go through Universal Clipboard. |
| D4 | The desktop's QR | `ligusenroll1:` + workspace id, the desktop's device id, a 32-byte one-time secret, an expiry. The secret travels only optically. |
| D5 | Binding | The phone's request carries its unchanged `ligusphone1:` code, a P-256 signature by the phone over the request, and an **HMAC keyed from the QR's secret**. A box, relay or member without the secret can't substitute a key, replay into another offer, or have a request accepted. One use, 5-minute expiry. |
| D6 | Authorization | The desktop certifies through the **same** `certify_phone_code` as paste: the same `DeviceCertificateAdded`, the same fold rules, the passkey every phone enrollment needs. The box writes nothing to any log. |
| D7 | Wire names | All new: `ligusenroll1:`, `ligusenrollreq1:`, `ligusenrollres1:`, HKDF salt `ligus-enroll-qr-v1`, contexts `ligus-enroll-request-v1\0`, `ligus-enroll-result-v1\0`, `ligus-enroll-relay-v1\0`, `ligus-enroll-answer-v1\0`. Nothing existing is renamed or changed. Golden vectors in `crates/ligus-core/tests/golden/phone_enroll_qr_v1.jsonl`. |

## 3. What this builds on (code-verified)

| Piece | Where | Used for |
|---|---|---|
| `PhoneCode` (`ligusphone1:`), `decode` (signature, label clamp, derived id) | `crates/ligus-runtime/src/phone_enroll.rs:120-186` | Carried unchanged inside the request; the desktop certifies exactly this |
| `check_device_id` (derived-id rule) | `phone_enroll.rs:109` | Box, desktop and phone |
| `ChatService::phone_standing` / `certify_phone_code` / `certify_phone` | `crates/ligus-runtime/src/chat_service/phone_sign.rs:330,350,37` | Desktop: the same certification as paste. `certify_phone` refuses a Service author or a delegated device (`:38-40`) |
| Fold rule for a phone certificate | `crates/ligus-core/src/authz/mod.rs:450-481`, presence rule `crates/ligus-core/src/authz/rules.rs:27-37` | Unchanged: enrolled on the config log by a trusted, non-phone, non-delegated device of the same account, with that account's passkey; no takeover of an existing id |
| Desktop confirm guard (fingerprint and workspace must match the preview) | `app/src/phones_ui.rs:74-90` | Reused for the QR path |
| Sealed, device-signed presence slots between a box and desktops | `crates/ligus-runtime/src/revocation_status.rs:1-19,177-289`; box loop `crates/ligus-cli/src/serve/mod.rs:1508-1541`; desktop reader `app/src/lib.rs:5196` | The transport for the request and the answer |
| Relay presence route (opaque `{device_id, data}`) | `relay/server.go:804-835` | Unchanged |
| Tunnel enrollment, E2 window binding, E3a device claim, 4040 | `crates/ligus-cli/src/serve/tunnel/session.rs:100-252`, `tunnel/device_bind.rs` | Provisional mode branches from `BindCheck::Unknown` (`session.rs:210`) |
| Serve auth (tunnel peer, operator, bearer) | `crates/ligus-cli/src/serve/api.rs:691-715` | The new routes' callers |
| iOS scanner (AVCaptureMetadataOutput, `.qr`) | `ios/Ligus/Views/QRScannerView.swift:1-148` | Reused; accepts `ligusenroll1:` in the add-phone context |
| iOS `PhoneCode`, `DeviceIdDerivation`, `PhoneLabel.clamp` | `ios/Ligus/PhoneActions/PhoneEnroll.swift` | Reused to build the request |
| Box admission (`ligusjoin1` → `ligusadmit1`) | `docs-site/docs/operate/ops/admitting-a-box.md`, `crates/ligus-runtime/src/box_admission.rs` | Pattern: request with public keys, fingerprint compared, passkey, one use, closes after settling |

Why not the alternatives:

- **Relay rendezvous (b).** The relay's short-code broker (`POST /v1/pair`,
  `GET /v1/pair/{code}`, `relay/server.go:878-995`, `sealed1:` payloads
  only, `relay/pairing_policy.go:35`) only lets the *creator* post and the
  *code holder* read: the phone could post, but the desktop can't learn the
  code the relay picks. Any other mailbox (presence, candidates) is
  workspace-scoped and, on a token-gated relay, refused to a caller with no
  relay token (`relay/server.go:495-513`); the phone has none. A real
  rendezvous route would be a relay change.
- **Desktop as a temporary tunnel host (c).** Needs `/v1/tunnel/*` enabled
  (off by default, `relay-tunnel.md` §15.2) and an allowlist entry on
  allowlist relays, and the tunnel host lives in `ligus-cli`, not the app.
- **Direct LAN listener on the desktop (c).** Fails across networks and
  client-isolated Wi-Fi, and triggers the macOS incoming-connection prompt
  and the iOS local-network prompt.
- **The box forwards every refused (4040) phone's code to the person's
  desktops, with no desktop QR.** Simpler, but the box then decides which
  key reaches the desktop; only the fingerprint comparison would stop a key
  substitution. The reverse QR's MAC takes the box out of the trust path for
  key integrity.

## 4. The flow

```
 Desktop (person P)                 Box (ligus serve, service member)          Phone
 ──────────────────                 ─────────────────────────────────          ─────
 Settings → Account → My devices
   → Add a phone
 secret s ← CSPRNG(32), exp = now+300
 show QR  ligusenroll1:{ws, D, s, exp}
                                                                       Add to my devices → scan
                                                                       (needs a channel to a box of ws:
                                                                        a pairing, or a provisional
                                                                        enrollment, §6)
                                     ◄─ POST /v1/phone-enroll/check {ws, D, rid, exp}
                                     ─► {workspaceName, person: P's name/role, desktop label}
                                                                       "Add this phone to P's devices
                                                                        in <workspace>?"  [Add]
                                     ◄─ POST /v1/phone-enroll/requests {request: ligusenrollreq1:…}
                                     checks (§7.3), then publishes presence slot
                                     phone-enroll:<rid>  (sealed, box-signed)
 polls presence, opens the slot,
 checks MAC (s), phone sig, code,
 offer, standing                                                       shows its fingerprint, waits
 "Ana's iPhone wants to be added"                                      (GET …/requests/<rid>?wait=25)
 fingerprint  ✓ same as the phone
 [Add this phone] → passkey
 certify_phone_code(ws, code)  ──► DeviceCertificateAdded (config log, synced as today)
 publishes phone-enroll-answer:<rid>
 (sealed, desktop-signed, MAC'd with s) ──►  box relays the answer
                                     box syncs the certificate             ◄─ answer: certified
                                                                       pairs normally (E3a) or,
                                                                       on the direct path, GET /v1/me
```

`D` is the desktop's workspace device id, `ws` the workspace id, `rid` the
rendezvous id derived from `s` (§5).

## 5. Wire formats

All integers little-endian, as in the phone-action preimage
(`docs/ios-app.md`, preimage layout). `b64u` is base64url without padding;
decoders refuse padding, `+`, `/`, and non-canonical trailing bits.

### 5.1 The desktop's QR: `ligusenroll1:`

```
offer      = "ligusenroll1:" + b64u( workspace_id(16) ‖ desktop_device_id(16) ‖ secret(32) ‖ exp(u64 LE, Unix seconds) )
           = 72 bytes → 96 characters after the 13-character prefix (QR version 7 at level M)
```

- The prefix is the version. A later format gets `ligusenroll2:`; there is no
  inner version byte.
- No display name, label or relay URL: what the phone shows about the
  desktop's person comes from the box's roster (§7.2), never from the QR,
  so a QR can't claim to be someone else's.
- Not a URL. The app's `ligus://` deep links are not extended: a web page
  or another app can't start this flow with a QR it made up. Only the
  in-app scanner (and paste into the phone's own field, for testing) reads
  it.

### 5.2 Keys from the secret

```
prk   = HKDF-Extract(salt = "ligus-enroll-qr-v1", ikm = secret)      (HKDF-SHA256, RFC 5869)
rid   = HKDF-Expand(prk, info = "rid",          16)
k_req = HKDF-Expand(prk, info = "request-mac",  32)
k_ans = HKDF-Expand(prk, info = "answer-mac",   32)
```

`rid` names the offer on the wire (the box and the relay see it); it
reveals nothing about `s`. `k_req` and `k_ans` never leave the desktop and
the phone.

### 5.3 The phone's request: `ligusenrollreq1:`

```
code_body  = the bytes of the phone's ligusphone1: code, unchanged:
             device_id(16) ‖ pubkey(65) ‖ code_sig(64) ‖ label(UTF-8, ≤ 64)

signed     = "ligus-enroll-request-v1\0" ‖ rid(16) ‖ workspace_id(16) ‖ desktop_device_id(16)
             ‖ exp(u64 LE) ‖ code_body
phone_sig  = ECDSA P-256 / SHA-256 by the phone's device key over `signed`, r ‖ s, low S (64)
mac        = HMAC-SHA256(k_req, signed ‖ phone_sig)                                      (32)

request    = "ligusenrollreq1:" + b64u( rid ‖ workspace_id ‖ desktop_device_id ‖ exp
                                        ‖ phone_sig ‖ mac ‖ code_body )
           = 233 + 145..209 = 378..442 bytes before encoding
```

- `code_body` decodes as a `ligusphone1:` code with every check of
  `PhoneCode::decode` (`phone_enroll.rs:150-171`): the code signature, the
  label clamp, the derived id. The desktop certifies that `PhoneCode`, so
  certification is byte-for-byte what the paste flow certifies.
- `phone_sig` is fresh: it binds this phone's live consent to this offer
  (`rid`, `ws`, `D`, `exp`). A `ligusphone1:` code someone saw earlier is
  not enough to answer an offer.
- `mac` proves the sender read the QR. The box can check everything except
  the MAC; the desktop checks everything.

### 5.4 The desktop's answer: `ligusenrollres1:`

```
outcome    = 1 certified | 2 declined | 3 refused (standing: conflict or revoked)
signed     = "ligus-enroll-answer-v1\0" ‖ rid(16) ‖ outcome(u8) ‖ device_id(16)
tag        = HMAC-SHA256(k_ans, signed)
answer     = "ligusenrollres1:" + b64u( rid ‖ outcome ‖ device_id ‖ tag )      (65 bytes)
```

`device_id` is the phone the desktop answered about. The phone checks the
tag and that `device_id` is its own.

### 5.5 Presence slots (sealed, signed)

Both follow `revocation_status.rs`: the blob is sealed under the keyring's
seal key at the authorized head (`seal_symmetric_kid`, no new AEAD domain),
and the sealed JSON is signed by the writer's device key over
`context ‖ canonical JSON without the signature` (`revocation_status.rs:35-39`).

| Slot | Field | Writer | Plaintext | Signature context |
|---|---|---|---|---|
| `phone-enroll:<b64u rid>` | `phoneEnrollRequest` | the box that took the request | `{request, relay: <box device id>, at}` | `ligus-enroll-relay-v1\0` |
| `phone-enroll-answer:<b64u rid>` | `phoneEnrollAnswer` | the desktop that showed the QR | `{answer, desktop: <device id>, at}` | `ligus-enroll-answer-relay-v1\0` |

Readers drop a blob that doesn't open at or after the authorized head, isn't
in the slot its `rid` names, isn't signed by a certified, unrevoked device
of a current member (`certified_device_key`,
`crates/ligus-runtime/src/chat_service/guards.rs:105`), or whose `at` is
more than 30 s ahead or 60 s old. The box re-publishes every 10 s while the
request is pending and writes `{}` when it settles; the desktop does the
same for its answer until the offer's expiry.

The relay sees two slot names per enrollment and the timing; the slot's
`rid` is unlinkable to the secret.

## 6. Reaching a box before the phone is certified

A phone pairs only once certified (E3a, `relay-tunnel.md` §7.2;
`session.rs:203-236`), so the phone that most needs this flow has no
channel yet.

- **Direct path.** A phone paired with a v1/v2 link that has `url` and
  `token` (tailnet or LAN) is paired before it is certified
  (`ios/Ligus/App/AppModel.swift:606-623`). It calls the routes with the
  bearer token.
- **Relay-only (tunnel).** New, additive, end to end between phone and host,
  invisible to the relay: **E3b, provisional enrollment.**

### 6.1 E3b: provisional enrollment (tunnel)

msg1's payload gains one optional field:

```
{"v":1, "name":…, "pk":…, "device":{"id":…, "sig":…}, "enrollKey":"<b64u 65-byte SEC1 P-256 key>"}
```

`EnrollHello` (`session.rs:100-109`) ignores unknown fields today, so an old
host answers 4040 as before, and an old phone never sends `enrollKey`.

Host rules, only where E3a would refuse with **4040 `device_not_certified`**
(`BindCheck::Unknown`):

- **P1.** `enrollKey` must be a valid point, `check_device_id(device.id,
  enrollKey)` must hold, and `device.sig` must verify under `enrollKey` over
  the same `ctx("device-bind", host_id, phone_static)`
  (`device_bind.rs:61`). Otherwise 4042.
- **P2.** The attempt counts against the window (`window_failed`,
  `tunnel/mod.rs:660`), as a 4040 does today; `ps` is not consumed and no
  phone record is created (E3/E4 don't run).
- **P3.** msg2 is `{"v":1,"provisional":true}`: no `pid`, no `cred`.
- **P4.** The transport is served by a **separate router** holding only
  `POST /v1/phone-enroll/check`, `POST /v1/phone-enroll/requests` and
  `GET /v1/phone-enroll/requests/{rid}`, with a `ProvisionalPeer {device_id,
  public_key}` extension. It never reaches the API router or its `auth`
  middleware (`api.rs:691`), which would refuse it anyway (no active `pid`).
- **P5.** A provisional connection may submit requests only for its own
  `device_id`, at most 8 requests, and lives at most 5 minutes (the relay
  already caps an enrollment splice at 5 minutes and 4 MiB,
  `relay-tunnel.md` §6.4.2). At most 4 provisional connections per box at
  once.
- **P6.** When the phone's certificate has synced, the phone closes the
  provisional connection and enrolls again with the same window: E3a now
  binds. The window allows 5 attempts (relay and host), so the budget is
  normally provisional (1) + final (1).

No relay change: the relay already passes enrollment splices through
without looking inside (§6.5), and msg1/msg2 payloads are end to end.

## 7. Box: routes and checks

### 7.1 Who may call

| Caller | Allowed | Device |
|---|---|---|
| Provisional tunnel peer (E3b) | yes | the one it proved (P1) |
| Paired tunnel peer | yes | its pairing's device (`TunnelPeer`) |
| Bearer (direct path) | yes | the request's own |
| Operator on loopback | yes (tests, CLI) | the request's own |
| Anonymous | no (401, as every route but `/v1/health`) | |

Nothing these routes return is secret: no `s`, no keys, no tokens.

### 7.2 `POST /v1/phone-enroll/check`

Body `{workspaceId, desktopDeviceId, rid, exp, deviceId?}` (`deviceId` only
off the tunnel). Writes nothing. Answers `200 {workspaceId, workspaceName,
person: {displayName, role}, desktopLabel, phone: {standing}}` where
`person` is the member who holds `desktopDeviceId` on this box's roster.
Refusals, `409 {error, message}`:

| `error` | When |
|---|---|
| `wrong_workspace` | `workspaceId` isn't this box's workspace |
| `desktop_unknown` | `desktopDeviceId` isn't a certified, unrevoked, non-phone, non-delegated device of exactly one current human member here |
| `offer_expired` | `exp < now − 120` or `exp > now + 900` (box clock) |
| `device_revoked` | the phone's device is revoked here (403) |
| `already_yours` | the phone is already certified for that person (200 with `standing: alreadyMine`, not an error) |
| `someone_elses` | the phone is certified here for another account |

### 7.3 `POST /v1/phone-enroll/requests`

Body `{request: "ligusenrollreq1:…"}`. The box decodes it and checks
everything it can:

1. the encoding (lengths, canonical b64u, prefix);
2. `code_body` decodes as a valid `ligusphone1:` code;
3. `phone_sig` verifies under the code's key over `signed`;
4. the caller's device (§7.1) is the code's device id, when the channel
   binds one;
5. every `check` rule above;
6. limits: at most one pending request per phone device (a new one replaces
   it), at most 8 pending in all, at most 30 accepted per minute.

It can't check `mac` (it never has `s`). Then it publishes the
`phone-enroll:<rid>` slot (§5.5) and answers `202 {rid, state:
"relayed"}`. Re-submitting the same bytes is idempotent; pending requests
live in memory until `exp` and are dropped on restart (the phone
re-submits on a 404 from the status route).

### 7.4 `GET /v1/phone-enroll/requests/{rid}?wait=N`

Long-polls up to `N ≤ 25` s. `{state: "waiting" | "answered" | "expired",
answer?: "ligusenrollres1:…", certified: bool}`. `answer` is the desktop's
slot, relayed as is (the box can't check its tag). `certified` is whether
this box's roster now has the phone's certificate.

### 7.5 What the box may write

Nothing on any log. It publishes and clears its own `phone-enroll:*`
presence slots, signed by its own device key. It can't certify a phone
(`certify_phone` refuses a Service author, `phone_sign.rs:38-40`, and the
fold refuses an enroller that isn't a device of the phone's account,
`authz/mod.rs:472-479`).

## 8. Desktop

### 8.1 The offer

**Settings → Account → My devices → Add a phone** (the button is renamed
from "Add this phone"). Before showing a QR the desktop checks that it can
certify at all: an active workspace, a relay presence connection
(`active_presence_conn`, `app/src/lib.rs:5166`), and this device is a
person's own, not delegated, with an account (the `certify_phone`
preconditions). Otherwise it opens straight on the paste flow and says why.

New Tauri commands (`app/src/phones_ui.rs`):

- `phone_enroll_offer_start()`: draws `s` from the OS CSPRNG, `exp = now +
  300`, keeps `Offer {ws, s, rid, k_req, k_ans, exp, state}` **in memory
  only** (one per desktop; a new one replaces the old), and returns `{qrSvg,
  rid, expiresAt}`. The QR is rendered in Rust (`qrcodegen`, already a
  dependency of `ligus-cli`), so the webview never holds `s` as text.
- `phone_enroll_offer_poll()`: lists presence (every 2 s while the panel is
  open), opens `phone-enroll:<rid>`, and checks, in order: the slot
  signature (a certified device of a member), `rid`, `mac` (constant time),
  `ws`/`D`/`exp` equal the offer, `exp` not passed (desktop clock),
  `phone_sig`, `PhoneCode::decode(code_body)`, `phone_standing`. Returns
  `waiting`, `request {label, fingerprint, deviceId, standing, via}`, or a
  refusal.
- `phone_enroll_offer_approve(fingerprint, rid)`: re-runs the checks, then
  `confirm_matches_preview` (`phones_ui.rs:74`, fingerprint and workspace),
  then `certify_phone_code(ws, &code)`: the passkey sheet names the
  fingerprint (`phone_enroll.rs` test `the_passkey_sheet_names_the_fingerprint`).
  Publishes the `certified` answer and closes the offer.
- `phone_enroll_offer_decline(rid)`: publishes `declined`, closes.
- `phone_enroll_offer_cancel()`: drops the offer.

**One use.** The first request that passes every check locks the offer to
its phone. Re-publications of the same bytes are ignored. A *different*
valid request for the same `rid` before approval aborts the offer: "Two
phones answered this code. Someone else may have seen it. Start again."

### 8.2 What the desktop shows

- The QR, "Scan with the Ligus app on your phone: Settings → This phone →
  Add to my devices", a countdown, and **Show a new code** when it expires.
- On a request: "**Ana's iPhone** wants to be added", relayed by
  *box-label*, the fingerprint in five groups, "Your phone shows the same
  fingerprint" checkbox, **Add this phone** (passkey), **Not my phone**.
- **Can't scan?** below: the existing paste panel, unchanged except its
  hint, which loses the camera-app sentence.

## 9. Phone (iOS)

- **Settings → This phone → Add to my devices** (primary). The code card
  moves under **Can't scan? Show this phone's code**.
- The pairing refusal screen for `device_not_certified` / provisional
  (`PairingRefusalView`, `ios/Ligus/Views/PhoneCodeView.swift:102-176`) leads with **Scan your
  desktop's code**.
- Scanner: the existing `QRScannerSheet`, which today takes only `ligus://`
  (`QRScannerView.swift:142`, `DeepLink.hasAcceptedScheme`), takes
  `ligusenroll1:` when opened from Add to my devices. Copy: "Point at the
  code on your desktop (Settings → My devices → Add a phone)".
- After the scan: decode, check `exp`, pick the host profile whose
  workspace (from `/v1/me` or `check`) is the offer's; if none, "Pair with
  this workspace's box first" and open the pairing scanner, keeping the
  offer in memory until it expires.
- Confirm: "Add this phone to **Ana**'s devices in **acme**?" (names from
  `check`), **Add**. Then: sign `signed` with the Secure Enclave key, build
  the request, submit, and show the fingerprint large with "Your desktop
  shows the same fingerprint. Check it there, then confirm with your
  passkey." and a waiting state.
- On `certified` (tag verified, own device id) and the box's `certified:
  true`: enroll again (tunnel) or refresh `/v1/me` (direct), then "This
  phone is now one of your devices".
- Errors: expired ("Show a new code on your desktop and scan again"),
  `declined`, `wrong_workspace`, `someone_elses`, `device_revoked` (offer
  Reset device key), box too old (404/4040 without provisional: "Update
  ligus serve, or use Can't scan?"), no relay/presence on the desktop
  (desktop side).
- `s`, `k_req` and `k_ans` live in memory only and are wiped after the
  flow; nothing is logged.
- `Info.plist` `NSCameraUsageDescription` (`ios/Ligus/Support/Info.plist:85`)
  becomes "Ligus uses the camera to scan pairing codes from your box and
  your desktop."

## 10. Authorization summary

- Only the person whose desktop showed the QR can certify: the desktop
  certifies under its own author and account (`phone_sign.rs:41-46`), and
  the fold requires a trusted, non-phone, non-delegated device of that
  same account (`authz/mod.rs:472-479`).
- The certificate is the paste flow's: same function, same event, same
  fold rules, same passkey requirement (`authz/rules.rs:33-37`). No new
  trust path: the request only moves the phone's code to the desktop.
- Roles: unchanged. The phone tier is capped by the person's role.
- Boxes write no events and hold no passkey; they relay ciphertext.

## 11. Accessibility

- Desktop: the QR image has `role="img"` and `aria-label="QR code to add a
  phone. Can't scan? Use the paste option below."`. The countdown is
  announced politely at 60 s and at expiry, not every second. The
  fingerprint has an `aria-label` that spells it out group by group. Focus
  moves to the request card when it appears.
- Phone: the scanner announces "Scanner. Point the camera at the code on
  your desktop"; a successful scan gives a haptic and a VoiceOver
  announcement. The fingerprint uses `.speechSpellsOutCharacters`. Dynamic
  Type throughout. Can't scan? is reachable without the camera.

## 12. Android parity (to add to `docs/android-app.md`)

- Feature matrix row: "Add to my devices: scan the desktop's
  `ligusenroll1:` QR (CameraX + ML Kit, QR only, in-app scanner only); the
  `ligusphone1:` code stays under Can't scan?".
- 2.7 Deep links: no `ligus://` link for enrollment; the scanner only.
- 2.11 Secure clipboard: the offer is never copied or shared.
- 2.12 `FLAG_SECURE` on the waiting screen (it shows the fingerprint).
- 4.1 Golden vectors: `phone_enroll_qr_v1.jsonl` (offer, keys, request,
  answer, negatives).
- E3b provisional enrollment in the tunnel client.

## 13. Tests

- **Rust unit and property** (`phone_enroll_qr.rs`): round trips; every
  single-bit flip, truncation and extension of offer, request and answer is
  refused; non-canonical b64u refused; `mac` and `tag` compared in constant
  time; a request under another offer's secret refused; a request with a
  valid MAC but another phone's code refused (phone_sig); label clamp
  inherited.
- **Golden vectors** `crates/ligus-core/tests/golden/phone_enroll_qr_v1.jsonl`
  (fixed secret, ids, key; RFC 6979 signatures from Rust; `rid`, `k_req`,
  `k_ans`, request, answer; negatives with their refusal strings), generated
  by an `--ignored` Rust test like `golden_coverage.rs`, copied into the iOS
  `LigusTests/Fixtures/golden/`.
- **Serve API** (`crates/ligus-cli/src/serve/tests.rs`): each 409/403 in
  §7.2; bad phone_sig; device mismatch on the tunnel; limits; idempotent
  re-submit; status long-poll; the slot is sealed and signed; no log write.
- **Tunnel** (`tunnel/e2e_tests.rs`): E3b happy path; P1 refusals (4042);
  the provisional router serves nothing else (404 for `/v1/chats`); old
  phone (no `enrollKey`) still gets 4040; window attempt accounting; then
  E3a binds after certification.
- **Web** (`AddPhonePanel.test.tsx`): QR shown, countdown, request card,
  checkbox gates the button, abort on a second phone, Can't scan? paste
  still works, the camera-app hint is gone.
- **iOS** (`RustParityTests`-style): a Rust-generated differential corpus
  `rust_parity_enroll_v1.jsonl` (mutated offers, requests and answers with
  the core's verdict) the Swift parser must match exactly.
- **End to end**: a real serve box with a fake relay, a desktop
  `ChatService`, and a simulated phone (testkit `Phone`, `PhoneKeys`): scan
  → provisional → request → desktop certify → answer → E3a pairs → `/v1/me`
  200, extending `add_this_phone_then_pair_and_ask_who_am_i`
  (`tunnel/e2e_tests.rs:1296`).

## 14. Owner questions

- **O1.** Let the `ligusphone1:` code go through Universal Clipboard (it
  holds no secret; the fingerprint comparison is the defence), to make the
  paste fallback work across devices? Recommended: yes.
- **O2.** Approve E3b (a tunnel-host protocol addition, no relay change).

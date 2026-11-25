# Code-match approval (design note)

Status: design only, not implemented. Target: Ligus 0.4. Surfaces: the
desktop, the phone apps, `lt` (ligus-term), the `ligus` CLI and `ligus serve`,
plus one additive route family on the relay (`langstroth-relay`). Ligus is
short for *ligustica*, the Italian honeybee.

Citations are `file:line` into this repository at `origin/main` `3c1638ddf`.
When this note and the code disagree, the code wins.

## 1. The problem

Three flows need a person on one device to approve something another device
asks for:

| Flow | Requester | Approver | What the approval writes |
|---|---|---|---|
| Admit a box | `ligus admit request` on the box | an Admin's desktop, phone or headless CLI | `JoinApproved` with a service admission, passkey |
| Add a computer to my devices | `ligus devices request` / lt | the person's desktop | `DeviceCertificateAdded` (delegated device), passkey |
| Add a phone to my devices | the phone app | the person's desktop | `DeviceCertificateAdded` (P-256), passkey |

Today each one moves a long base64url blob by hand (paste, or the phone's
reverse QR). The person then compares a 20-character hex fingerprint
(`7F3A-91C0-77DE-0B14-5E62`) and, for a computer, compares the approving
desktop's fingerprint a second time and pastes a second blob back. Nothing
reaches the approver by itself. A box join request or a computer link request
never appears in the Inbox or on the phone. An Admin who isn't at the box
needs the request sent to them some other way, usually a chat message, which
is the same channel an attacker would use. (The Inbox's `device` row is the
legacy "a box says it's your device" notice, not a request to approve.)

Prior art (authenticator "number matching", Bluetooth numeric comparison,
short authentication strings in secure calls) settles on one idea: **both
screens show the same short code, derived from the exchange itself, and the
person confirms they match.** A substituted key or a request relayed for
someone else then shows a different code, and the person needs no hex.

## 2. What exists (code-verified)

- **Box join request** `ligusjoin1:` (`crates/ligus-runtime/src/box_admission.rs:25,37`):
  public keys only (account id, Ed25519 device key, X25519 key-agreement
  key, label). It carries **no signature and no time**. Its fingerprint is
  `SHA256("ligus-box-join-v1\0" ‖ account ‖ device ‖ keys)[..10]` (`:69`).
  The approval is `JoinApproved` with an `Admission` bound to that account and
  device key, and it always needs a passkey (`crates/ligus-core/src/authz/rules.rs:13-24`).
  The code `ligusadmit1:` is sealed to the box's key-agreement key
  (`e2ee::seal`) and redeemable for one hour. Desktop: `web/src/components/AdmitBoxModal.tsx:115`.
  Phone: `/v1/admissions/prepare|presence|commit` on the box
  (`crates/ligus-cli/src/serve/admissions_api.rs`). The phone checks the
  preimage before it signs (`crates/ligus-core/src/admission.rs:408`).
- **Link request and grant** (`crates/ligus-runtime/src/computer_link.rs`,
  `docs/design/link-a-computer.md` §4): `liguslink1:` is self-signed and
  15 minutes old at most. `ligusgrant1:` is sealed to the request's
  key-agreement key, signed by the approving desktop, and valid for 30
  minutes. The computer confirms the approver's fingerprint before it saves
  anything (review finding M4 there). By design the grant **never goes
  through the relay** (§4.2 there).
- **Phone codes** (`crates/ligus-runtime/src/phone_enroll.rs`,
  `phone_enroll_qr.rs`, `docs/design/v2/phone-enroll-reverse-qr.md`): the
  `ligusphone1:` paste path, and the reverse-QR path, where a 32-byte optical
  secret keys an HMAC over the phone's request. Its answer travels through
  sealed, signed presence slots via the workspace's `ligus serve` box. There
  is a 5-second hold before the desktop's approve (`HOLD_MS`) and a tripwire
  if a second phone answers.
- **Device fingerprint**: `device_enroll::fingerprint`
  (`crates/ligus-runtime/src/device_enroll.rs:164`), 80 bits, upper-case hex
  in five groups, rendered by `web/src/components/kit/Fingerprint.tsx`. The
  owner deferred a word form (`docs/design/devices-and-keys.md`).
- **Passkey presence**: the challenge is a digest of the drafted event
  (`crates/ligus-core/src/presence.rs:245`). The desktop's sheet
  (`web/src/components/PresenceSheet.tsx`) shows the kind, a summary and a
  16-hex `digestShort`. **Gap:** the summary for a box admission is generic
  ("Let someone join with a single-use invite") and names neither the box nor
  its fingerprint (`crates/ligus-runtime/src/chat_service/presence.rs`
  `summarize`).
- **Phone Face ID** (`docs/ios-app.md:768-780`): every phone signature except
  `post` asks for a fresh Face ID (reuse duration 0), with a reason naming the
  action.
- **Inbox** (`web/src/lib/needsYou.ts:49-60`): kinds in order `phone`,
  `sandbox`, `trust`, `ask`, `vote`, `apply`, `failed`, `device`, `invite`.
  Nothing is approved from the Inbox itself (`web/src/components/Inbox.tsx:18-20`).
- **Push** (`docs/push-notifications.md`, `crates/ligus-cli/src/serve/tunnel/push.rs`):
  only through a paired `ligus serve` box, sealed per phone. Kinds include
  `workflow.approval` and `phone.paired`; there is none for admissions or
  device requests. The desktop gets no push; it learns through sync.
- **Encodings**: base64url everywhere, BIP-39 for recovery words only. There
  is **no base32, no Crockford encoder and no short-code generator**.

## 3. Goals and non-goals

Goals:

1. One short code, the same on both screens, for every live approval of a
   new device or box. The approver confirms a match with their passkey (Face
   ID on the phone) in the same step.
2. The code **binds both ends**: the requester's keys and request, and the
   approver's device. It comes from a commit-then-reveal exchange, so neither
   a relay nor anyone else can steer it, even with offline computation.
3. Requests **reach the approver** in the Inbox and, through a paired box, as
   a phone push, with no blob to paste. Paste stays as the fallback.
4. A mismatch grants nothing, ends the request, and tells both sides.
5. Calm: one line, large, grouped; no new banner.

Non-goals:

- Changing who may approve what. The fold rules, presence rules and events
  are unchanged.
- Replacing the 80-bit fingerprint where no live channel exists (a pasted
  request from an offline box): it stays there (section 6.6).
- Bearer admissions for Kubernetes (`--ttl`, no request): there is no screen
  at the pod, and they are unchanged.
- Sign-in to a relay through langstroth-auth: its device-code page has its
  own user code (an RFC 8628 code typed at the IdP, not a comparison).
  Ligus never shows that code next to a match code (section 8, threat T6).
- Approving a computer or phone **from the phone**. The certificate must be
  published by a non-delegated, non-phone device that holds the account key
  (`link-a-computer.md` §3). See open question Q3.

## 4. The design in one picture

```
requester (box / new computer / phone)          approver (desktop / phone via box)
  1. post sealed request R + commit C=H(nR) --> relay mailbox --> Inbox, push
                                                  2. person opens it; device picks nA
  3. <-------------------- nA, approver key, signed ------------------------
  4. reveal nR, signed ---------------------------------------------------->
  both: code = Crockford32( H(transcript ‖ nA ‖ nR) )[40 bits] -> "K7Q4 M9X2"
                                                  5. "Codes match" + passkey -> event on the log
  6. <-------------- grant sealed to R's key (through the mailbox) ---------
  7. requester checks the grant names the same transcript, then redeems as today
```

## 5. The approval mailbox (transport)

### 5.1 Address

A workspace has one **approval address**:

```
ligusaddr1:<base64url(JSON {v:1, relay, workspaceId, founderPin, epoch, mailbox, sealKey, putCap})>
```

- `workspaceId` and `founderPin` (`FounderPin::fingerprint`,
  `envelope_verifier.rs:367`, the same value invites pin) name the real
  workspace. **A requester never redeems a grant whose connection names
  another workspace id or founder pin** (6.3). This makes a grant pointing at
  a fork useless, even one built by someone who holds the workspace key.
- `epoch` is the workspace key epoch the address was derived from.
- `mailbox` is self-certifying. Members derive an Ed25519 **mailbox auth
  key** from the epoch key (`HKDF-SHA256(ikm = the workspace key of that
  epoch, salt = "ligus-approval-mailbox-v1", info = "auth")`), and `mailbox`
  is the first 16 bytes of `SHA256(authPub)`. The relay accepts register and
  reset only when they are signed by that key, so nobody without the
  workspace key can claim or reset a mailbox, on any relay. It is **not** the room
  id: a room id is kept in a Secret for cluster installs (AGENTS.md), and the
  relay can't link a mailbox to a room.
- `sealKey` is the X25519 public key derived the same way (`info = "seal"`).
  Every member that holds that epoch's key derives the private half; the
  relay can't read what's posted.
- `putCap` is 16 bytes. A member registers its hash on the relay with the
  mailbox (`PUT /v1/mailboxes/{mailbox}`, signed by the mailbox auth key). A
  request must present it. **Reset address** registers a new `putCap` (the
  relay keeps a counter, and members derive `putCap = HKDF(epoch key,
  "ligus-approval-mailbox-v1", "put" ‖ u32le(counter))`), so a leaked address
  can be cut off without a key rotation. Every key holder can sign a reset,
  so "only Admins reset" is a client-side convention: the desktop offers it
  to Admins only. A member resetting it out of turn only forces a re-copy of
  the address.
- **Epochs.** A key rotation (`WorkspaceKeyRotated`) gives a new address.
  Members keep polling the previous two epochs' mailboxes, so a stale
  address in a script or manifest still reaches honest approvers. Those
  approvers answer with the current key. A device revoked before a rotation
  can still derive the old mailbox. It can read public request data and race
  an `Open` (T7), but it can't produce a grant the requester accepts (6.3).
- The address **is not a secret and grants nothing**. Leaking it lets someone
  drop requests into Admins' Inboxes, which the limits in 5.3 and Reset
  address bound. It is shown with Copy in the desktop (workspace menu > Admit
  a box, and My devices), printed by `ligus admit address`, and included in
  the throwaway workspace a trial makes.

### 5.2 Messages

Every message is a `SealedBlob` (`e2ee::seal`, the existing X25519 + HKDF +
ChaCha20-Poly1305) and is signed inside the seal. All contexts are new.

| Message | Sealed to | Signed by | Body |
|---|---|---|---|
| **Request** `ligusreq1` | `sealKey` | requester device key (Ed25519; P-256 for a phone) | `{v, kind: box\|computer\|phone, rid(16 random), code: <the unchanged ligusjoin1/liguslink1/ligusphone1 text>, requesterKa, readKey, commit, createdAt}` under `"ligus-match-request-v1\0"` |
| **Claim** | `sealKey` (members see it) | approver device key | `{rid, approverDeviceId, at}` under `"ligus-match-claim-v1\0"`: "this device is checking it" |
| **Open** | `requesterKa` | approver device key | `{rid, approverDeviceId, approverSigKey, approverKa, approverPerson, nA(32), at}` under `"ligus-match-open-v1\0"` |
| **Reveal** | `approverKa` | requester | `{rid, nR(32)}` under `"ligus-match-reveal-v1\0"` |
| **Result** | `requesterKa` | approver (`approverSigKey`) | `{rid, outcome: approved\|mismatch\|declined\|expired, grant?, grantDigest?}` under `"ligus-match-result-v1\0"`. `grant` is the existing `ligusadmit1:` or `ligusgrant1:` text, sealed as it is today; `grantDigest` is its SHA-256 |
| **Abort** | `requesterKa`, and `sealKey` | anyone | `{rid, reason: mismatch\|cancelled\|contested}` under `"ligus-match-abort-v1\0"`. An abort only ever ends a request, so the requester accepts one from anyone (at worst, denial of service) |

- `requesterKa` is an X25519 key in the request, covered by its signature.
  A box or a computer uses the key-agreement key its code already carries. A
  **phone uses a fresh ephemeral key**, because `ligusphone1` has none
  (`phone_enroll.rs:56-62`).
- `approverSigKey` (Ed25519 or P-256) signs; `approverKa` (X25519, ephemeral
  per exchange) receives the reveal. Both go in the transcript.
- `readKey` (16 random bytes) is the requester's read capability on the relay
  (5.3). It isn't a member, and on a gated relay it has no token, so it reads
  and writes its own `rid` with this key.

A box join request has no signature today. Inside a `ligusreq1` the box's
device key signs the whole request, `ligusjoin1` included, which proves it
holds that key. The `ligusjoin1` codec itself doesn't change.

A phone that has a box to talk to posts through the box's existing presence
slots (`revocation_status.rs`, the reverse-QR transport). A phone with no box
posts to the relay mailbox like any other requester.

**Who is "the approver" when a phone admits a box.** It is the **phone**. The
phone generates `nA` and `approverKa`, signs `Open` and `Result` with its
P-256 device key, and computes the code itself. The serve box only carries
bytes:

1. The phone receives the request's raw `ligusreq1` plaintext. The box can
   open it, being a member. The phone parses the `ligusjoin1` bytes itself
   and checks the request signature.
2. It picks `nA` **only after** it holds the commit, and checks
   `commit == H(rid ‖ nR)` itself when the reveal arrives.
3. It checks that the `/v1/admissions/prepare` preimage admits exactly the
   keys it parsed (today's `check_phone_admission_preimage`,
   `admission.rs:408`, compared with its own parse, not the box's preview).
4. The `ligusadmit1` code the box builds after commit is sealed to the box
   requester. The phone can't read it, so it signs `grantDigest` in `Result`.
   The requester checks that digest, and then the log (6.3).

A box that rewrites any byte changes the code the phone shows. It can't pick
`nA` or see `nR` early, so it can't grind.

### 5.3 Relay routes (additive, `langstroth-relay`)

```
PUT    /v1/mailboxes/{mailbox}                      register or reset: {authPub, putCapHash}, signed by the mailbox auth key; returns the counter
PUT    /v1/mailboxes/{mailbox}/requests/{rid}       one request, <= 8 KiB; needs putCap; registers H(readKey)
GET    /v1/mailboxes/{mailbox}/requests             list (opaque blobs, ids, times); a member
POST   /v1/mailboxes/{mailbox}/requests/{rid}/msgs  claim, open, reveal, result, abort; <= 32 KiB each
GET    /v1/mailboxes/{mailbox}/requests/{rid}/msgs?after=N&wait=S
DELETE /v1/mailboxes/{mailbox}/requests/{rid}       the requester (readKey), once it has the result
```

Limits, enforced by the relay:

- at most 64 live requests per mailbox (a 16-box rollout fits) and 10
  messages per request;
- a request expires 15 minutes after it was put, and its messages with it;
- 10 puts per minute per client address;
- no overwrite of an existing `rid`.

**Auth.**

- **A member** (list, claim, open, result) uses its relay access token on a
  gated relay, and nothing on an open one.
- **A requester** isn't a member. It presents `putCap` to put, and `readKey`
  for its own `rid`'s messages (read, reveal, delete). This works on gated
  relays too, so a new computer with no token can take part (it had none
  before linking, `link-a-computer.md` §4).
- **On a gated relay** an operator can additionally require a relay access
  token for puts (`mailboxes: token`). Without the routes, or with
  `mailboxes: off`, the relay answers 404 and Ligus falls back to paste
  (section 10).

The only unauthenticated writes are puts that present a valid `putCap`, which
an Admin can reset (AGENTS.md rule 2).

**Retention.** The result message holds a sealed copy of the workspace and
epoch keys. If the requester's key-agreement key leaked later, that copy
would be readable, so it is kept as short as possible: the requester deletes
the `rid` as soon as it has the result. The relay drops it at 15 minutes
otherwise.

## 6. The code

### 6.1 Derivation (normative)

```
commit     = SHA256("ligus-match-commit-v1\0" ‖ rid ‖ nR)
transcript = SHA256("ligus-match-transcript-v1\0"
               ‖ u8 kind ‖ mailbox ‖ rid ‖ SHA256(request code bytes)
               ‖ put(requesterKa) ‖ commit
               ‖ approverDeviceId ‖ put(approverSigKey) ‖ put(approverKa) ‖ nA)
code40     = first 40 bits of SHA256("ligus-match-code-v1\0" ‖ transcript ‖ nR)
shown      = Crockford base32 of code40: 8 symbols, two groups of four, "K7Q4 M9X2"
```

`put` is a u32le length followed by the bytes. The request's code bytes hold
its keys (device, key-agreement, account for a box), its label and its own
signature for a computer or phone. The transcript therefore covers the
requester, the approver and both nonces.

**Why commit-then-reveal and not a hash of the request.** A 40-bit code over
static data can be ground: an attacker who sees an honest request can try
around 2^40 keys of its own, minutes on one GPU, until one gives the same
code, then substitute it. With the commitment, the requester fixes `nR`
before it sees `nA`, and the approver picks `nA` before it sees `nR`. A
man in the middle runs two exchanges and must make two codes equal that each
depend on a nonce it learns only after its own choice is fixed. The chance is
2^-40 per attempt, and every attempt ends the request (6.4). This is the
argument behind Bluetooth numeric comparison and secure-call short
authentication strings.

**Why 8 symbols.** 40 bits gives one-in-a-trillion odds per attempt with one
attempt allowed. Six digits (20 bits) is what authenticators use for number
matching, but there the code only proves attention, not key binding.
Crockford base32 has no I, L, O or U, ignores case on input, and reads
unambiguously aloud.

### 6.2 What the approver's confirmation binds

The approver's device writes the event exactly as today (`JoinApproved`, or
`DeviceCertificateAdded` through `certify_phone_code` or `add_computer`), over
**the keys from the request whose transcript produced the code**. It never
uses keys from a second copy. The passkey ceremony is the existing one. Its
sheet summary gains the label and the code ("Admit box build-1, code K7Q4
M9X2"), which closes the generic-summary gap (section 2). On the phone,
Face ID's reason says the same. Nothing about the code goes on the log (open
question Q4).

### 6.3 What the requester checks

Before it writes anything, the requester checks these things:

1. The `Result` is signed by `approverSigKey` from the `Open`, and its
   `grantDigest` equals the SHA-256 of the grant it carries.
2. The grant answers this request: a computer's `ligusgrant1` names the
   request digest, and a box's admission is bound to this device key.
3. **The grant's connection names the address's `workspaceId` and
   `founderPin`.** A grant built by anyone holding the workspace key, but
   pointing at another relay or founder (a fork), is refused here. This also
   covers link-a-computer R5 whenever the requester came in through an
   address. Its key epoch must be at least the address's `epoch`.
4. For a computer, the approver key in the grant equals `approverSigKey`.
5. **After syncing with the pinned founder** (boxes sync first anyway,
   `admitting-a-box.md`), the event that approves it must be on the log,
   signed by the device whose key is `approverSigKey`. That device must be
   certified, unrevoked, and of an Admin (box) or of the person (computer).
   A box admission must also carry passkey presence. The requester retries
   this check for up to 2 minutes while the approval syncs. Otherwise nothing
   is redeemed, and it exits 1 with "the approval wasn't found on the
   workspace log".
6. **A computer confirms the verified person.** After check 5, it shows
   "Link this computer as **Ana** (verified on the workspace log)? [y/N]"
   (lt: **Yes, link** / **No**). This is link-a-computer M4's person check,
   moved after verification. It is needed because any member could race an
   `Open` and approve from their own desktop. A box needs no such step: it
   joins as its own service member.

Check 5 is what lets a **headless box** accept without a person at it (7.4).
Someone who isn't an Admin, or a device revoked before the last rotation, can
race an `Open` and make the box show their code. But they can't put a valid
`JoinApproved` signed by an Admin device on the real log, so the box never
joins through them. The rest of redeem is unchanged.

### 6.4 Attempts, expiry and replay

- **One attempt per request.** A requester accepts the first `Open` for a
  `rid` and ignores later ones. After any abort, mismatch or decline the
  request is dead, and a new one needs a new `rid`, `nR` and commit. The CLI
  stops after 3 dead requests in 15 minutes and says why (exit 1).
- **Expiry.** The request lives 15 minutes in the mailbox. After `Open`, the
  code is valid for **5 minutes**. Both screens show a quiet countdown
  (`kit/Countdown`), and at zero both drop the code. The grant keeps its
  current life (admission 1 h, computer grant 30 min).
- **Replay.** Each approver device keeps the `rid`s it has opened, per
  mailbox, for 24 hours, and refuses a reused `rid` or a request older than
  15 minutes. The requester keeps its pending `rid` with `nR` in its data dir
  (owner-only, like `computer-link.json`) and deletes it once settled. A
  replayed `Open` carries a fresh `nA` that no screen shows. Signatures are
  domain-separated per message, so none can be swapped for another.
- **Two approvers, or a racing one.** An approver posts a `Claim` (sealed to
  members) before its `Open`. The requester answers the first `Open` only.
  - A second approver's sheet names who claimed it: "Ben's desktop is
    checking this request".
  - When the claim is missing, or is by a device that can't approve this
    kind (not an Admin's for a box, not the person's for a computer, revoked,
    unknown), the sheet says "Opened by a device that can't approve this. It
    may be an attempt to intercept it." and offers **End this request**,
    which posts `Abort{contested}`.
  - The requester shows the `Open`'s claimed person and device as
    unverified, "Says it's Ana's desktop (checked after approval)", and
    check 5 in 6.3 settles it.
- **Several requests at once.** Each has its own `rid` and its own code, so a
  parallel request (an attacker's, timed to match the person's) shows a code
  the person's device doesn't. The sheet says "2 devices are asking. Check
  the code on the one you're holding." Nothing is refused wholesale, because
  that would let one junk request block every device addition.

### 6.5 When the codes don't match

Either side chooses **Codes don't match**. That side posts `Abort{mismatch}`,
and the other side shows the same outcome. An abort is unauthenticated to
the requester, so the requester shows who sent it only as a claim. On
approvers, the intercept warning below appears only for this device's own
mismatch, or for an abort signed by a device of the workspace, which it
names. Nothing is written to any log and
nothing is granted. The approver's device records a device-local fact. The
Inbox keeps one row for a day: "Request from build-1 didn't match. Nothing
was granted.", with a "?" explaining that someone may have tried to
intercept it and what to do (check the device you're adding, start again,
and tell an Admin if it happens twice). The requester prints the same, exits
1, and tells the operator to start again on a trusted network.

### 6.6 Paste, offline, and the fingerprint

The fingerprint **stays**, and the code **augments** it:

- **Live (both ends reach the mailbox):** the code is primary. The
  fingerprint sits under **Details** in the sheet and in `--json`, and still
  shows in My devices afterwards.
- **Paste path with a reachable requester:** a pasted `ligusjoin1` or
  `liguslink1` text has no `rid`. A 0.4 requester prints `ligusreq1`-aware
  text that adds `rid` and `commit` as an optional suffix field. When the
  approver can reach the mailbox it runs the same exchange, and the code
  replaces the fingerprint compare.
- **No live channel** (an older requester, or a relay without the routes):
  the 80-bit fingerprint compare, exactly as today. A 40-bit code over static
  data is never shown (6.1).

Removing the fingerprint compare from the live path everywhere is a later
step (open question Q5).

## 7. UX

All copy below is the exact proposed text.

### 7.1 The code itself (every surface)

- **One line, large, mono, two groups**: `K7Q4 M9X2`. Desktop 28px, phone
  Dynamic Type `largeTitle` mono. It never shrinks below body size and never
  wraps inside a group.
- **No Copy button**, on purpose. The code is for comparing, not for sending.
  Only the requester's terminal prints it as text, because a terminal has no
  other way.
- **Screen readers** read it in groups, letter by letter. Desktop:
  `aria-label="Code: K, 7, Q, 4. M, 9, X, 2."`, with the same
  `spellFingerprint`-style helper extended to Crockford. iOS: an
  `accessibilityLabel` with `.speechSpellsOutCharacters`. Android:
  `contentDescription` with a `TtsSpan.VerbatimBuilder`. The groups are
  separate accessibility elements, so a person can step through them.
- **Not colour alone**: match or mismatch is always said in words.
- **Shown during screen sharing**, unlike fingerprints in the Add a phone
  panel. The code is not a secret (T4), and a person helping remotely needs
  to see it. Only what is secret (a grant, a pairing link) stays hidden.

### 7.2 Desktop (approver)

- **Inbox row** (new kind `approval`, ranked right after `phone`, among the
  security rows): one line. "build-1 wants to join" or "lt on mbp wants to be
  your device", muted time on the same line. Clicking the row opens the
  sheet. Nothing is approved from the row itself.
- **The sheet** (`kit/Sheet`, centred), title "Admit build-1?" or "Add lt on
  mbp to your devices?":
  1. The code, large, one line.
  2. One line beneath it: "Check the box shows the same code." For a device
     of mine: "Check the device you're holding shows the same code."
  3. Primary **Codes match**: it enables after a 3-second hold from when the
     code appears, then runs the passkey ceremony. Secondary **Don't match**.
  4. **Details** (collapsed): kind, label, fingerprint, when it was asked,
     the approval address's relay. A "?" beside the title explains what a
     match proves, for screen readers too.
- The countdown is one muted number in the sheet's corner. There is no new
  banner, and the design-QA scan's one-banner rule holds.
- **Admit a box** and **My devices > Add a computer / Add a phone** gain the
  approval address with Copy. Paste stays as a tab.

### 7.3 Phone (approver for boxes; viewer for devices)

- **Push** (new kind `approval.request`, sealed per phone like every push,
  sent only by a paired box): "build-1 wants to join <workspace>". Pushes
  collapse per workspace. When more than 3 requests arrive in 10 minutes, the
  phone stops pushing and the Inbox shows one row, "Several join requests",
  so a flood can't wear the person down.
- **Approval screen**: the code, one line; "Check the box shows the same
  code."; **Codes match**, then Face ID, whose reason is "Admit build-1, code
  K7Q4 M9X2"; **Don't match**. This uses the existing
  `/v1/admissions/prepare|presence|commit`. The serve API gains
  `GET /v1/approvals` (each request's raw `ligusreq1` plaintext) and
  `POST /v1/approvals/{rid}/claim|open|result|abort`, which carry
  phone-signed messages. The **phone** is the approver: it makes `nA` and its
  ephemeral key, checks the commit and the parsed keys, and computes the
  code, as set out in 5.2. The box only relays bytes.
- **A device of mine** (computer or phone) shows on the phone as read-only
  for 0.4: "Approve on your desktop", with the code, so the person can
  compare it against both (Q3).

### 7.4 The requester: CLI and box

```sh
ligus admit address [--json]                                   # on any member: prints ligusaddr1:
ligus admit request --to ligusaddr1:… [--label NAME] [--wait 15m] [--json]
ligus devices request --to ligusaddr1:… [--label NAME] [--wait 15m] [--json]
```

- `--to` posts the request and, **straight away**, also prints the paste text
  and the fingerprint, so an older desktop can approve without waiting. Then
  it waits (stderr: "Waiting for an approver…").
  When an approver opens it, the box prints one line on stderr,
  `Code: K7Q4 M9X2  (the approver's screen must show the same)`, and with
  `--json` a `checks[]` entry `{id: "match-code", status: "pass", detail: "K7Q4 M9X2"}`.
  The code is not a secret, so it may appear in output.
- **At a terminal**, the box asks "Does the approver's screen show this
  code? [y/N]" before it accepts the grant. **Without one** (systemd, a
  pod), a box accepts the grant only after the checks in 6.3, check 5
  above all: the approval must be on the real log, signed by an Admin's
  device with a passkey. The Admin's match and passkey are the human step. A
  **computer** without a terminal exits 4 instead; it acts as a person, so a
  person must confirm on it.
- Exit codes (the shared table): 0 approved and redeemed; 1 mismatch,
  declined, expired or dead; 2 usage; 3 relay without mailbox routes ("paste
  the request instead", `fix` names the paste command); 4 waiting on a person
  past `--wait`, or a computer with no terminal; 7 relay unreachable.
- **Headless approver** (`ligus approvals list --json`, `ligus approvals
  approve <rid> --code K7Q4M9X2 --recovery-words-file PATH`): a person's
  step, like `ligus admit issue`. `--code` must equal what this device
  computes. The CLI never offers to fill it in. Without `--code` it exits 4.

### 7.5 lt

- **As requester** (Add to my devices): one framed line with the code,
  "Check your desktop shows the same code.", then **Yes, they match** /
  **No**. This replaces the second paste and the approver-fingerprint step
  whenever the live path is used.
- **As approver**: lt is a delegated device and can't certify devices. It can
  admit a box only through the browser passkey, like any governance act. In
  0.4 lt lists approval requests read-only ("Approve on your desktop or
  phone") with the code.

## 8. Threat model

| # | Threat | What stops it | Residual |
|---|---|---|---|
| T1 | **Relay man in the middle** substitutes keys in the request, or answers as a fake approver | The transcript covers both keys and both nonces. A substitution shows different codes (2^-40 per attempt, one attempt). The request is sealed, so the relay can't read labels. The grant checks in 6.3 | None beyond 2^-40 |
| T2 | **Phishing the approver**: an attacker posts its own box or computer and asks a person to approve it, reading out "the code" | The code proves only that the two screens are in the same exchange. It can't tell whose device is at the other end. For devices of mine, the copy says "the device you're holding" and no Copy exists. Several requests at once each show their own code. The sheet names the kind and the label plainly, and the Inbox shows the request as "wants to be your device" | **Residual R1.** A person who approves a request someone else reads to them is phished, as today. Prior art accepts this; number matching (Q2) narrows it for rushed approvals, not for deceived ones |
| T3 | **Rushed approvals and fatigue** (repeated pushes until someone taps yes) | 3-second hold; push collapse and a stop after 3 in 10 minutes; one attempt per request; Admin-only for boxes; passkey or Face ID every time | Q2 offers number matching on the phone |
| T4 | **Shoulder-surfing** | The code is not a secret: seeing it gives nothing, and it can't be typed anywhere to gain access | None |
| T5 | **Inbox spam** through a leaked address | `putCap` and Reset address; relay limits (64 live, 10/min per client); optionally a token for puts on gated relays; one Inbox row per request, collapsing past 3 | Between a leak and a reset, spam can fill the mailbox, which blocks live approvals (paste still works) |
| T6 | **Code confusion**: an RFC 8628 sign-in code, a recovery word or a pairing link mistaken for a match code | Different shape (8 Crockford symbols in two groups) and different words ("code to compare"). A match code is never asked to be typed except by `ligus approvals approve --code`, a person's own CLI | None |
| T7 | **A key holder who can't approve** (a Viewer, a service box, a device revoked before the last rotation) races an `Open`, or sends a grant pointing at a fork | It can make an interactive requester show its code, which no approver shows (T1). The `Claim` rule flags it to Admins. A grant fails 6.3 checks 3 and 5: the founder pin is wrong, or there is no Admin-signed approval on the real log | Denial of service on that request |
| T8 | **A compromised serve box** relaying for the phone | The phone is the approver. It makes `nA` after it holds the commit, checks the commit, parses the request bytes itself, checks the admission preimage against its own parse, and signs `grantDigest` (5.2) | The box can withhold or delay requests (as today) |
| T9 | **A grant from another workspace** answering a public request (link-a-computer R5) | 6.3 check 3: the grant must name the address's workspace id and founder pin | Paste path keeps today's human check |
| T11 | **A stale address** (in a manifest) after a rotation | Members poll the previous two epochs; check 3 needs a key epoch at or above the address's; check 5 | Past two rotations the address is dead and the requester exits 7 then 4 ("ask an Admin for the current address") |
| T10 | **Replay** of a request, an open or a result | One `rid` per exchange, 24-hour seen-set, per-message contexts, 15-minute life, pending state deleted once settled | None |

## 9. Rules and events

- **No new log event, no new field on any log event, no rules change.**
  `JoinApproved` and `DeviceCertificateAdded` are written exactly as now. Every
  released peer folds them as today.
- New wire names, none renamed: `ligusaddr1:`, `ligusreq1`, HKDF salt
  `ligus-approval-mailbox-v1`, contexts `ligus-match-request-v1\0`,
  `ligus-match-claim-v1\0`, `ligus-match-open-v1\0`, `ligus-match-reveal-v1\0`, `ligus-match-result-v1\0`,
  `ligus-match-abort-v1\0`, `ligus-match-commit-v1\0`,
  `ligus-match-transcript-v1\0`, `ligus-match-code-v1\0`. Push kind
  `approval.request`. Inbox kind `approval`. Golden vectors go in
  `crates/ligus-core/tests/golden/match_code_v1.jsonl`, copied byte for byte
  to the phone repositories.
- The grant travels through the relay sealed, **only after a code match**.
  This reverses link-a-computer's "never through the relay" for the live path
  (open question Q1). The paste path is unchanged.

## 10. Mixed versions

| Requester | Approver | Relay | Result |
|---|---|---|---|
| 0.4 | 0.4 | with routes | Code match |
| 0.4 | 0.4 | without routes (404) | Paste, fingerprint; the CLI exits 3 with the paste command in `fix` |
| 0.4 | older desktop | any | The older desktop never polls the mailbox. The requester has already printed the paste text and fingerprint (7.4), which the older desktop accepts as today; past `--wait` it exits 4 |
| older | 0.4 | any | Paste, fingerprint, as today |
| 0.4 phone | older box | any | The phone sees no `/v1/approvals` (404) and keeps today's flows |

A 0.4 desktop never weakens a fingerprint compare to make an older peer
work. It falls back to the full compare.

## 11. Tests (executable, required before merge)

| # | What | Where |
|---|---|---|
| M1 | Golden vectors: commit, transcript, code, Crockford encoding (and decoding with I/L/O mapped, case-insensitive); the same vectors pass in Rust, Swift and Kotlin | `ligus-core` `match_code`, phone repositories |
| M2 | Changing any one of kind, mailbox, rid, a request byte, commit, approver id or key, `nA` or `nR` changes the code (property test) | `match_code::tests` |
| M3 | A simulated relay MITM (two exchanges, attacker controls both nonces in the allowed order) never yields equal codes over 10^6 runs with the code truncated to 16 bits, and the observed rate matches 2^-16; with 40 bits nothing matches | `match_code::tests` |
| M4 | A reveal that doesn't match the commit aborts; a second `Open` is ignored; a reused `rid` is refused; a request older than 15 min is refused; the code expires 5 min after Open | `approval_mailbox::tests` |
| M5 | Mismatch on either side: nothing on any log (fold diff empty), the Inbox fact exists, the requester exits 1 | `approval_tests` end to end on the test relay |
| M6 | Grant checks: wrong approver key, a wrong `grantDigest`, a grant for another device, a fork (the real workspace key, another relay or founder pin), an older key epoch, and an approval not on the log or signed by a non-Admin or revoked device are all refused, and nothing is written | `approval_tests` |
| M6b | Headless box capture: a Viewer, a service member and a device revoked before rotation each race an `Open` and a grant; the box never joins, and the Admin's sheet shows the contested claim | `approval_tests` |
| M6c | The serve box as the man in the middle for a phone approval: rewriting the request bytes, the commit, `nA` or the preimage either changes the phone's code or fails a phone check; the box never sees `nR` before `nA` is fixed | `serve` tests, phone tests |
| M6d | A member races an `Open` for Ana's computer and approves from their own desktop: the computer shows "Link as <that member> (verified)" and links nothing on No | `approval_tests` |
| M7 | Two device requests at once each show their own code; approving one leaves the other pending; two box requests each get their own code | `approval_tests` |
| M7b | Relay: puts without `putCap` refused; Reset address invalidates the old cap; a requester with `readKey` and no token reads only its own `rid`; delete on settle | `langstroth-relay` tests |
| M8 | Box: no terminal accepts after checks; a computer with no terminal exits 4; `--json` is one `ligus.agent-deploy.v1` document with no secret (scan for `hivews1`, `ligusadmit1`, `ligusgrant1`) | `ligus-cli` |
| M9 | Mixed versions: an older requester (paste) and an older desktop (no polling) complete through paste; a relay without routes gives exit 3 | `approval_tests`, `deploy/tests` |
| M10 | Relay limits: 65th request refused, 11th message refused, expiry, no overwrite; register and reset refused unless signed by the mailbox auth key; puts need `putCap` (and a token under `mailboxes: token`) | `langstroth-relay` tests |
| M11 | UI: the code has the spelled `aria-label`; Codes match is disabled for 3 s; no Copy; design-QA scan passes with the sheet open (one banner at most, AA contrast) | `web` Vitest, design-qa |
| M12 | The passkey sheet summary and the phone's Face ID reason name the label and the code | `chat_service::presence` tests, phone UI tests |

## 12. Open questions for the owner

| # | Question | Recommendation |
|---|---|---|
| Q1 | May the grant (`ligusadmit1`/`ligusgrant1`, sealed to the requester) travel through the relay mailbox after a code match? It removes the second paste, but reverses a rule in link-a-computer, and the relay briefly holds a sealed copy of the workspace keys (5.3, Retention) | **Yes**, only after a match, deleted as soon as the requester has it. The match binds the key it's sealed to. Paste stays |
| Q2 | Number matching on the phone: the phone shows three codes and the person taps the one on the other screen, instead of "Codes match" | **Yes for push-started approvals on the phone**, where fatigue lives. Desktop keeps compare and confirm |
| Q3 | Approve a computer or phone from the phone (the phone as the passkey for a certificate the desktop drafts) | **Not in 0.4.** It needs a desktop online to publish anyway. Show it read-only on the phone with the code |
| Q4 | Record the transcript digest on the log (an optional field on `JoinApproved`) for audit | **No.** Keep the logs unchanged in 0.4; a device-local fact is enough |
| Q5 | When to drop the fingerprint compare from the live path entirely | **After one release** with both, once mismatch telemetry (local facts only) shows no confusion |
| Q6 | Open relays: allow anonymous mailbox puts at all? | **Yes, with `putCap`, Reset address and the limits**, since otherwise open-relay users keep pasting. Gated relays may also require a token for puts (`mailboxes: token`) |

## 13. Review

One adversarial reviewer, read-only, two passes against the code at
`3c1638ddf`.

**First pass: Revise.** The commit-then-reveal argument held. The binding
of each end did not.

| # | Finding | Severity | Resolution |
|---|---|---|---|
| R1 | A serve box relaying for a phone could choose `nA` or see `nR` early and grind the 40-bit code | Must-fix | The phone is the approver: it makes `nA` after it holds the commit, checks the commit, parses the request bytes and checks the preimage itself; the box only relays (5.2). Test M6c |
| R2 | Sealing keys didn't exist: `ligusphone1` has no X25519 key; `approverKey` conflated signing and key agreement; a phone can't sign a grant the box seals | Must-fix | `requesterKa` (ephemeral for a phone), `approverSigKey` and `approverKa` both in the transcript, `grantDigest` (5.2, 6.1) |
| R3 | A headless box auto-accepting could be captured by any key holder with a grant pointing at a fork | Must-fix | The address pins `workspaceId` and `founderPin`; check 5 needs an Admin-signed, passkey-backed approval on the real log before redeem (6.3). Tests M6, M6b |
| R4 | After a rotation, a stale address became a mailbox only revoked devices read | Must-fix | Address epochs; members poll the previous two epochs; epoch check; T11 |
| R5 | Racing `Open`s by any member | Should-fix | `Claim`, contested sheets and End this request; the requester shows the claimed approver as unverified (6.4) |
| R6 | Mailbox DoS; refuse-both let one junk request block every addition; a 16-box rollout hit the cap | Should-fix | `putCap` and Reset address; 64 live; no refuse-both (5.1, 5.3, 6.4) |
| R7 | A new computer on a gated relay can't read its own `rid` | Should-fix | `readKey` (5.2, 5.3) |
| R8 | The relay holds a sealed copy of workspace keys | Should-fix | Delete on settle; stated in Q1 (5.3) |
| R9 | An older desktop meant waiting 15 minutes before seeing the paste text; the Inbox `device` claim | Nit | Paste text printed straight away; section 1 corrected |
| R10 | Hiding a non-secret code blocks remote help | Nit | Shown during screen sharing (7.1) |

**Second pass: Approve with risks**, every first-pass Must-fix resolved, no
new Must-fix. Folded in:

| # | Finding | Severity | Resolution |
|---|---|---|---|
| S1 | Anyone could register or reset a mailbox on an open relay; "Admin reset" wasn't enforceable | Should-fix | Self-certifying mailbox: `mailbox = H(authPub)`, with register and reset signed by the mailbox auth key; Admin-only reset stated as a client convention (5.1) |
| S2 | A member could race an `Open` and link Ana's computer as their own device | Should-fix | Check 6: a final confirmation of the verified person after check 5 (6.3). Test M6d |
| S3 | Anyone's abort raised the intercept warning | Nit | Shown only for the device's own mismatch, or a named workspace device's abort (6.5) |
| S4 | Check 5 raced sync | Nit | A 2-minute retry (6.3) |
| S5 | Stale text from the first draft | Nit | Fixed (T2, section 10, M10, Q6) |

**Third pass: Approve**, no Must-fix. Accepted: any member can derive
the mailbox auth key, so any member can reset the address, which is denial
of service only.

**Residual risks.** R1 in section 8: a person who approves a request someone
else reads out to them is phished, as today. Denial of service by a key
holder or a leaked address until Reset address. The relay briefly holds a
sealed copy of the workspace keys (Q1).

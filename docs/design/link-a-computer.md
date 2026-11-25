# Link a computer as one of your devices (design note)

Status: design, built with this note (revised after the adversarial review in section 11). Targets: the `ligus` CLI, the desktop
(Settings, Profile, My devices) and `lt` (ligus-term). Ligus is short for
*ligustica*, the Italian honeybee.

## 1. The problem

Someone creates a workspace in the desktop app, then invites themselves into
`lt` (or `ligus enroll` plus `ligus join`). Every invite makes a **new
member**: the workspace now has two people with the same name, two roles, two
sets of votes, and the desktop's chats are not `lt`'s. The phone already has
the right shape ("Add to my devices": a device certificate under the person's
own account, confirmed with their passkey). A second computer has no such
path: the desktop's "Add a device" still points at `ligus serve enroll`,
which 0.1 removed (boxes join as their own service member through
`ligus admit`).

## 2. What exists (code-verified)

Line references are to `origin/main` at `e025226ad`.

- **Device certificates.** `DeviceCertificate { device_id, device_public_key,
  account_id, issued_at, signature, alg }` (`ligus-core/src/crypto.rs:416`),
  signed by the account key, published as `DeviceCertificateAdded` on the
  workspace config log. An envelope stamped as account `A` and signed by
  device `D` verifies iff a valid `A -> D` certificate is in the log and `A`
  is a member (`envelope_verifier::verdict_for`). A certificate is a *trust
  bootstrap*: accepted inert from an unknown device and judged by the roster
  (`envelope_verifier.rs:890`).
- **Who published it decides the device's standing** (`authz/timeline.rs:425-450,
  590-600`): a certificate published by the device it certifies (the
  envelope's signer is that device and verifies under the key the
  certificate names) is *self-published*; one published by another certified,
  non-delegated device of the account is an *enrollment* (sticky). A desktop
  or box enrolled by someone else is a **delegated host**: it may post, but
  its votes need the owner's passkey presence, and it may not apply
  proposals, request or withdraw applies, re-pin a chat's base, share files
  from its own device, register passkeys or publish identity links
  (`authz/mod.rs:766, 821, 853, 914, 963`, `attachments.rs:130`,
  `passkeys.rs:58`, `links.rs:33`). A **phone** (P-256 key) is never
  delegated; it has its own tier.
- **Presence.** Under rules v2 a new device of an account that holds a
  passkey needs that account's passkey presence over the certificate event
  (`authz/rules.rs:33-37`, `authz/mod.rs:612-621`). The WebAuthn challenge is
  `presence::challenge(env)` (`presence.rs:245`): workspace, log, event id,
  lamport, payload, config frontier and the stamped account. **It does not
  cover the signer, the signature, the sequence or the chain link.**
- **The old box enrollment** (`device_enroll.rs`, `hivedev1:`): the owner's
  desktop published the box's certificate itself, so the box became a
  delegated host, and the box's former member became a *retired alias*
  whose history reads as the owner's. Removed for boxes in 0.1
  (`admit.rs:37`). Its runtime pieces (`ChatService::act_as_owner_device`,
  `device_standing`, `Standing::OwnerDevice`) remain and are tested.
- **Box admission** (`box_admission.rs`): the box prints a join request
  (public keys), an Admin approves with a passkey, and the desktop hands back
  the workspace connection sealed to the box's X25519 key-agreement key
  (`ligusadmit1:`, `e2ee::seal`).
- **Revocation.** `DeviceRevoked`; self-service for a device your account
  holds (R5, `authz/mod.rs:680`), with presence; every member then refuses
  the device's later events. My devices lists `devices_of(account)` with
  Revoke (`app/src/lib.rs:5164`, `AccountSection.tsx` `DevicesSection`).
- **One author per service.** `ChatService` has one `author`; the CLI uses one
  workspace per data dir; `lt` uses one service for all its workspaces.

## 3. Decision: reuse the certificate as is; the desktop publishes it

**No new event, no new field, no new rule.** The new computer becomes a
device of the person with an ordinary `DeviceCertificateAdded` for its own
Ed25519 key under the person's account, **published by the person's
desktop** (the device that holds the account key) with the person's passkey
presence. Every released peer (0.1.0 and later) folds that as an
*enrollment*: the computer is a **delegated device** of the person, the
shape the rules already have for a machine that acts for someone without
their account key (`timeline.rs` `enrolled_by_other`, `delegated_for`).
On the computer, `ChatService::act_as_owner_device` (the existing, tested
runtime mode for exactly this) stamps the person and refuses every
account-key act locally.

What the linked computer does as the person: post, read, start chats and
runs, answer, lock a chat, be a host that applies a proposal a person asked
it to apply, run agents bound to it, publish its key-agreement key, revoke
itself. What stays with the desktop (the rules refuse it from a delegated
device, on every version): a vote without the person's passkey (lt asks for
one in the browser, as for any governance act), applying a proposal itself or
asking a host to, sharing a file from its own disk, registering passkeys,
identity links, certifying devices or phones, rotating keys, founding a
workspace, signing invites as founder.

**Why not a key-less device that publishes its own certificate** (the first
draft of this note): the computer would fold as a non-delegated device of
the person without holding the account key, which the rules assume never
happens. The review showed two consequences no fold change could fix for
older peers: such a device could re-publish the desktop's certificate and
make the desktop delegated forever (finding M1), and the rules give it the
powers of a key holder. Making a key-less "own device" safe needs a
rules-version bump (section 12, decision D1). Copying the account key to the
computer is rejected outright (revoking a device couldn't take back what it
can sign).

## 4. Codes

Both codes are plain text to copy, paste or send to yourself. **Neither
grants anything by itself.**

### 4.1 Link request (`liguslink1:`), printed by the new computer

`base64url(JSON)` of:

| Field | What |
|---|---|
| `v` | 1 |
| `deviceId` | the device id its key derives (`crypto::device_id_for`); any other is refused on decode |
| `devicePublicKey` | Ed25519, 32 bytes |
| `kaPublic` | X25519 key-agreement key, 32 bytes: the grant is sealed to it |
| `label` | a hint ("lt on mbp"); controls and bidi/invisible characters dropped, 64 bytes |
| `createdAt` | unix ms |
| `previous` | optional: `{accountId, signature}`, see section 7 |
| `signature` | Ed25519 by the device key over `"ligus-link-request-v1\0" ‖ u32le(v) ‖ deviceId ‖ key ‖ kaPublic ‖ i64le(createdAt) ‖ label ‖ previous` (proof of possession; the version is inside it) |

The **fingerprint** both sides show is the device fingerprint
(`device_enroll::fingerprint`: first 10 bytes of SHA-256 of the device key,
`XXXX-XXXX-XXXX-XXXX-XXXX`), the same one My devices shows afterwards. The
signature binds the key-agreement key, the label and the time to that key,
so a request swapped in transit shows another fingerprint, and one with any
field replaced fails to decode.

The request is public. The desktop refuses a request older than **15
minutes** or more than five minutes in the future. The new computer keeps the
request's digest (SHA-256 of what was signed and the signature) as
**pending** in its data dir (`computer-link.json`, owner-only), and accepts a
grant only for a pending request, once.

### 4.2 Grant (`ligusgrant1:`), shown by the desktop

`base64url(JSON SealedBlob)`: `e2ee::seal` to the request's `kaPublic`
(X25519 + AEAD; only the requesting computer opens it). Plaintext JSON:

| Field | What |
|---|---|
| `v` | 1 |
| `request` | the digest of the request it answers |
| `workspaceId` | the workspace |
| `conn` | the workspace connection (`hivews1:` codec: relay, room, workspace key, epoch keys, founder pin). **No invite id, no redeem key**: nobody is admitted |
| `deviceId` | the device the desktop certified (the request's) |
| `person` | the person's identity as the desktop stamps it |
| `approverDeviceId`, `approverKey` | the desktop that approved |
| `expiresAt` | unix ms; **30 minutes** after approval |
| `signature` | Ed25519 by the approver's device key over `"ligus-link-grant-v1\0" ‖` everything above |

**Approving** (`ChatService::issue_computer_link`): the desktop checks the
request is fresh and not its own device; it is a human member of the
workspace, holds the account key the workspace pins for the person, and is
itself a confirmed, non-delegated device; the workspace isn't in lockdown;
the account holds a credential (a passkey or recovery key); the request's
device isn't certified here for anyone (yours: "already your device";
anyone else's: "already in the workspace as someone else") or revoked. Then
it appends the certificate on the config log (the rules ask the person's
presence for a new device of an account with a credential: **one
ceremony**), and seals the grant. The certificate syncs from the desktop
like any event.

**Redeeming** (`ChatService::check_computer_link`, writes nothing): the grant
opens, answers a pending request of this install, hasn't expired, certifies
this device, names a human; its approver signature verifies. The computer
**shows the person, the workspace and the approving desktop's fingerprint,
and the person confirms** before anything is saved (the desktop's sheet
shows the same fingerprint as "This desktop"; section 11, M4). Then it pins
the grant's founder, pulls the workspace and checks: the founding is
verified; the person is a human member; the approver is a certified,
unrevoked, non-delegated, non-phone device of the person whose key is the
grant's; this device is certified for the person and for no one else, and
isn't revoked; no lockdown. A missing certificate means the desktop hasn't
synced yet: retry. On success the pending request is used up and the install
records that it acts for the person (`LinkState::record_link`).

**Workspace key delivery.** The workspace key and epoch keys travel only
inside the sealed grant; the grant never goes through the relay. Right after
redeeming, the computer publishes its own key-agreement key on the config
log, so later rotations seal to it like any device of a member. A rotation
between approval and that publication doesn't seal to it: the computer then
can't read past that rotation, and the fix is to add it again (residual R4).

**Relay access token.** Never in either code. A gated relay asks the new
computer to sign in as the person (`ligus auth login`) or to have
`LIGUS_RELAY_ACCESS_TOKEN` set there; redeem fails with the relay's refusal
and that fix.

## 5. UX

### 5.1 The new computer

**lt.** First run gains a fourth choice, **Add to my devices**:

1. lt shows the request code, the fingerprint in five groups, and "On your
   desktop: Settings > Profile > My devices > Add a computer. Paste this code,
   check the fingerprint, confirm with your passkey. Then paste the code it
   shows here."
2. A field for the grant code. lt opens it and shows "Approved by <name>'s
   desktop <fingerprint> for workspace <name>. Is that you and your desktop?"
   with **Yes, link** and **No**.
3. "This computer is now one of <name>'s devices in <workspace>."

The palette gets **Link this computer to my devices**: the same screens for
an install that already has an identity. An install that has already written
anywhere asks with a **new device key**, prepared next to the current one and
switched to only when a grant is redeemed (a request never answered changes
nothing). It can be linked only if the one workspace it holds is the grant's
(that entry is replaced); with other workspaces, use another data dir
(`LT_DATA_DIR`).

**CLI** (noun-verb, non-interactive, `--json` prints one
`ligus.agent-deploy.v1` document):

```sh
ligus devices request [--label NAME] [--new-device] [--json]          # on the new computer
ligus devices redeem <ligusgrant1:…> | --code-file PATH | - \
      [--approver-fingerprint XXXX-…] [--json]                         # on the new computer
ligus devices approve <liguslink1:…> | --code-file PATH --fingerprint XXXX-… \
      --recovery-words-file PATH [--out PATH] [--json]                # a person's headless approval (prefer the desktop)
ligus devices list [--json]                                           # your devices, and where this one stands
```

`redeem` without `--approver-fingerprint` asks at a terminal and otherwise
exits 4 (a person must compare it). `approve` without `--fingerprint` asks at
a terminal and otherwise exits 4; its recovery words are the person's
presence, so it is a human step (AGENTS.md, human-only steps), like
`ligus phones certify`. Exit codes (the shared table): 0 done; 1 a check
failed (expired, wrong device, not pending); 2 usage; 3 a fixable
precondition (no relay URL); 4 a person must act; 6 refused by the
workspace (lockdown, revoked, someone else's device); 7 the relay
unreachable or the desktop's approval not there yet. `ligus help devices`
documents it all.

### 5.2 The desktop

Settings > Profile > **My devices** has **Add a phone** and **Add a
computer** (replacing the stale "Add a device" and its `ligus serve enroll`
text):

1. Paste the computer's code. The sheet shows its label, the fingerprint in
   large mono groups ("Compare all five groups with the computer's screen.
   Only add a computer you are using right now."), and, when the request
   proves it, "This computer was the member **<name>** (a separate member) in
   this workspace."
2. "The computer shows the same fingerprint", then **Add with passkey**: one
   ceremony ("Add device <id> (key <fingerprint>) as one of your devices").
3. The grant code to copy, "Paste it on the computer within 30 minutes",
   "This desktop: <fingerprint>" (the computer shows it too), and **Done**.
   The computer is listed under My devices at once and can be revoked from
   there, linked or not.
4. If the request named a previous member: **Remove <name>…** (the ordinary
   member removal, its own passkey confirmation). "Their past messages keep
   their name."

A legacy `ligus serve` host that still names you as its owner (the old
`LIGUS_SERVE_OWNER`) shows the admission guidance instead: "Boxes join as
their own member now: on it run `ligus admit request`, then approve it under
workspace menu > Admit a box."

## 6. Rules and refusals (fail closed)

- **Who may add a computer**: any human member, for themselves, at any role;
  the computer carries their role. Never a box, an agent or a delegated
  device (no account key).
- **Lockdown (dearth mode)**: refused on both sides while the workspace is in
  dearth mode (a device certified during it would be held pending a second
  Admin anyway). A chat lockdown applies to the computer as to the person.
- **Already here**: refused when the request's device is certified in the
  workspace for anyone or revoked there. An install that has been in the
  workspace as its own member always asks with a new device key, so its old
  device (which signed as the old member) is never certified for the person,
  and nothing is re-attributed (`retired_aliases` needs exactly that).
- **One person per install**: a linked install acts as that person in every
  workspace it holds; it refuses a grant for another person, and refuses to
  create a workspace, accept an invite or become a box.
- **Revoked**: My devices > Revoke (R5: your own device, your passkey). Every
  member drops the computer's later events; it sees `Standing::Revoked` and
  stops writing. Re-linking takes a new device key (`--new-device`).
  **Revoking doesn't take back what it already read; to cut reading too,
  rotate the workspace key** (the existing revoke copy says so).

## 7. The duplicate member you already have

1. On the desktop: Settings > Profile > My devices > Add a computer.
2. In lt: palette > Link this computer to my devices. lt is already in that
   workspace as its own member, so it asks with a new device key and adds the
   `previous` proof: a signature by **that member's account key** (lt holds
   it) over the request.
3. The desktop's sheet says "This computer was the member <lt member>".
   Approve; paste the grant into lt; confirm the desktop's fingerprint.
4. The desktop offers **Remove <lt member>…**. Removing keeps every message
   they signed under their name (Ligus never re-attributes signed history)
   and refuses anything later signed as them. A removal needs an Owner's or
   Admin's passkey, ranked above that member (the owner of the workspace can;
   a Contributor can't remove their own duplicate, an Admin does it).

The `previous` proof only lets the desktop *say* "this was you"; it changes
no rule. A proof that doesn't verify under that member's registered account
key, or names a non-member, shows nothing.

## 8. Mixed versions

- Older members (0.1.0 to 0.2.x) fold the certificate with the rules they
  already run (they are unchanged since 0.1.0): the computer is the person's
  delegated device on every version. A member that hasn't pulled the
  certificate yet holds the computer's events (unknown device) until it
  arrives; nothing is accepted on the stamp alone.
- An older lt or CLI can't make or redeem these codes; an older desktop has
  no Add a computer. Either side refuses a code version it doesn't know.
- Nothing is renamed. The new prefixes and signature contexts are pinned by
  tests.

## 9. Setup flows

The guided setup engine (`ligus_runtime::setup`, in flight) registers this as
flow **`device`** ("Add a computer"): `request` (Input: label) → `approve`
(Person: on the desktop) → `redeem` (Input: the grant) → `confirm` (Person:
the approver's fingerprint) → verify (the computer acts as the person).
Until the engine lands, `computer_link::setup` carries the id, labels and
steps, and the steps are plain functions (`LinkRequest::new`,
`LinkState`, `Grant::open`, `OpenedGrant::check`,
`ChatService::check_computer_link`).

## 10. Tests (executable)

| # | What | Where |
|---|---|---|
| T1 | Request round trip; tampered key-agreement key, label, time, key, id or version refused; fingerprint equals the device fingerprint; contexts pinned | `computer_link::tests` |
| T2 | Grant opens only on the requesting device; tampering breaks the approver signature; expiry; another device's key, not a person, another workspace's connection refused; the desktop's own identity fields aren't copied | `computer_link::tests` |
| T3 | End to end through the test relay: approve, redeem, post as the person on the desktop and a teammate; the computer is delegated, its vote needs a passkey, one member not two, it publishes its key-agreement key and never an account key | `computer_link_tests` |
| T4 | A teammate's grant makes the computer the teammate's; a grant naming someone else than its approver's person is refused; the desktop refuses a device that is someone else's | `computer_link_tests` |
| T5 | Revocation: every member drops the computer's later events; it sees `Revoked` | `computer_link_tests` |
| T6 | Mixed versions: no `ligus-core` change, so every fold in the tests is the released one | (by construction) |
| T7 | Lockdown refuses; already-linked refused; the previous-member proof shows only when it verifies; the old member's history keeps its author after removal, no alias | `computer_link_tests` |
| T8 | The computer re-publishing the desktop's certificate leaves the desktop non-delegated (review M1) | `computer_link_tests` |
| T9 | Single use: a second redeem of the same grant is refused | `computer_link_tests` |
| T10 | CLI: `devices request`, `approve`, `redeem`, `list`, exit codes and `--json`; lt end to end with a throwaway workspace | `ligus-cli`, `ligus-term` |

## 11. Adversarial review

First pass on the first draft (key-less self-published device): **Revise**.

| # | Finding | Severity | Resolution |
|---|---|---|---|
| M1 | A key-less self-published device can re-publish the desktop's certificate and make it delegated forever | Must-fix | Design changed: the desktop publishes the certificate, the computer is delegated (section 3). Test T8 |
| M2 | A device that acts for an account without its key, with its Ed25519 key on disk, can re-publish its certificate, self-certify under a throwaway key and register it, disputing the account (its events then quarantined in that workspace) | Must-fix for the first draft | Confirmed on the second pass for every such device: legacy delegated boxes today, linked computers now; a phone only if its app is compromised (its Secure Enclave key signs allow-listed actions only). This design turns that class from a legacy-only path into a routine one. Residual R1 and decision D1 |
| M3 | No runtime mode for a key-less own device | Must-fix | The computer uses `act_as_owner_device` (delegated); `publish_identity` returns early; test T3 asserts no `AccountKeyRegistered`/`AccountKeyRotated` from it; the desktop's key-agreement key and contact id aren't copied |
| M4 | A grant from someone else's workspace answering a public request | Must-fix | Redeem shows the person, the workspace and the approving desktop's fingerprint and needs the person's yes (CLI: `--approver-fingerprint` or a terminal, else exit 4) |
| S1 | Phishing the approval; long request life | Should-fix | Request life 15 min; "only a computer you are using right now"; the previous member is shown as a separate member. Notifying other devices after a link is follow-up F1 |
| S2 | A grant can't be cancelled; revoke before redeem | Should-fix | The certificate is on the log at approval, so My devices lists the computer at once and Revoke works before or after it links; revoke copy says reading needs a key rotation |
| S3 | Equivocation on retry of the re-signed envelope | Should-fix | Gone: the computer signs nothing to link |
| S4 | Rotation between approval and key-agreement publication | Should-fix | The computer publishes its key at redeem; the window is the grant's life; residual R4 |
| S5 | Contradiction about installs with workspaces; who removes the old member; old device key reuse | Should-fix | Section 5.1 and 6 (only the grant's workspace may be held; always a new device key after writing); section 7 (who removes) |
| S6 | The legacy `hivedev1:` certify path | Should-fix | Its desktop UI is replaced by Add a computer; it now makes a delegated device, like this design, so it grants nothing more |
| S7 | Headless approve with recovery words | Should-fix | A person's step (help and AGENTS.md); needs `--fingerprint` or a terminal, else exit 4 |
| N1 | Phone enrollment picking a linked computer as the desktop | Nit | Already excluded (`!delegated_for`) |
| N2 | Unpushed `log_head` on the desktop | Nit | Gone with S3 |
| N3 | The grant in `--json` and logs | Nit | `approve` prints it only as its result (or to `--out`, 0600); never logged |

Second pass on the revised design: **Approve with risks**, no Must-fix.
Should-fix items folded in: T8b (a second copy of the certificate signed by
the computer leaves it delegated), account-key acts refused on the computer
(test), the desktop's `device_id` not copied, the revoke text asks an Admin
to rotate the key, residual R5.

### Security review of the code

**Approve with risks**, no Must-fix (a failed redeem was proved not to
publish a bridge). Fixed: a failed redeem no longer leaves the data dir
connected (the connection is saved only once the link is recorded, the pin
is dropped); a linked install refuses to open any workspace it isn't linked
into (it would be its old identity under the person's device key); `serve`,
`worker` and `pair` refuse a linked install before marking it a box; a
pending request lives as long as a grant for it can (15 + 30 minutes);
"connected" also counts an existing identity with a database; stdin and
names from a code are bounded. Kept: `remove_duplicate_member` uses the
ordinary member-removal rules; the grant's plaintext strings aren't
zeroized beyond the JSON buffer; unused prepared device seeds stay in the
vault.

## 12. Residual risks and decisions

- **R1.** A linked computer (or a legacy delegated box), if stolen and
  driven by hand, can dispute the person's account key in that workspace
  (M2); a phone only if its app is compromised. Revoking it and an Admin's key
  rotation recover reading; undoing the dispute needs D1.
- **R5.** Reverse phishing: someone who sees a computer's request (it is
  public) can answer it with a grant from their own workspace; every log
  check passes there. The only control is the person confirming the person,
  the workspace and the approving desktop's fingerprint before linking.
- **R2.** A stolen linked computer acts as the person (posts, reads, runs
  agents bound to it) until revoked; governance acts need the person's
  passkey.
- **R3.** Phishing the approval is a human check (fingerprint, "a computer
  you are using").
- **R4.** A key rotation in the 30 minutes between approval and redeem
  leaves the computer unable to read past it; add it again.
- **D1 (owner decision).** A rules-version bump (v3) that (a) lets only a
  device that registered the account's current key itself enroll others and
  raise a dispute, and (b) adds a "linked device" tier that may apply and
  share files without the account key. Peers on rules v2 go read-only on a
  v3 workspace until updated. Not built here.
- **F1 (follow-up).** Notify the person's other devices and phone when a
  computer is added, with Revoke.

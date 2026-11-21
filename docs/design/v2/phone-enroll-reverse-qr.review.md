# Review: phone-enroll-reverse-qr.md (add a phone by scanning the desktop's QR)

Reviewer stance: adversarial architecture review (the owner's reviewer
persona). Reviewed against the code on `origin/main` (`b635b65f9`), the relay
at `a5cec7b` and the iOS app at `38f18bb`. Ligus is short for *ligustica*.

Two rounds. Round 1 reviewed the draft (commit `c90babaa1`) and asked for
revisions. Round 2 reviews the revised design.

---

# Round 1 (draft `c90babaa1`)

## Decision

**Revise.**

The shape is right: the box as the return path, the presence channel as the
mailbox (no relay change, no new event kind), the unchanged `ligusphone1:`
code inside the request, and the HMAC keyed from a secret that travels only
optically. Certification through the same `certify_phone_code` adds no
trust path. But the draft leaves four holes that matter (R1, R2, R4, R5) and
several gaps that would surface as stuck or confusing flows (R6 to R10).

## Claimed invariants

- I1. Without the QR's secret, nobody (box, relay, another member) can get a
  key of their choosing in front of the desktop as "the phone".
- I2. A request answers exactly one offer, once, before its expiry.
- I3. The certificate is the paste flow's, written only by a trusted device of
  the person whose desktop showed the QR, with their passkey.
- I4. A box writes no events and can't certify.
- I5. No relay change; `/v1`, `hrt1` and the tunnel's relay layer unchanged.
- I6. Old desktops, phones, boxes and relays keep working; the new pieces
  fail closed to paste.
- I7. The secret never leaves the desktop and the phone (never to the box,
  relay, logs).

## Hidden assumptions

- H1. Only boxes write `phone-enroll:*` slots. Not enforced: the draft's
  reader accepts a slot signed by *any* certified device of a member (§5.5),
  and every member holds the seal key.
- H2. The person compares the fingerprint. The draft's only defence after a
  stolen QR, but the desktop stops listening once it has approved.
- H3. A display name identifies a person. Display names aren't unique.
- H4. A box's pending list is filled only by real phones. The box can't check
  the MAC, and the draft accepts an `exp` up to 15 minutes ahead.
- H5. The box always has a presence connection. A box on a workspace with no
  relay (P2P-only sync, direct path only) has none.
- H6. The desktop's answer always arrives. A desktop crash between the
  certificate and the answer leaves the phone waiting until expiry.
- H7. The QR is seen only by the person. Screen sharing, a screenshot tool or
  a recording sees it while the panel is open.

## Counterexamples

### C1. Stolen QR, approval before the real phone arrives (H2)

- **Claim:** "a different valid request ... aborts the offer" (§8.1).
- **Sequence:** M photographs the QR during a screen share. M has a channel
  that can reach the desktop's slot (H1: M is a member, or holds the box's
  bearer token, or a `ligus pair` secret). M submits M's own phone code with
  a valid MAC. The desktop locks onto it and shows "Ana's iPhone" (M chose
  the label). Ana, holding her phone that's still on the scanner, approves
  without reading the fingerprint. Then Ana's real request arrives.
- **Expected:** someone notices before or right after the wrong key is
  certified.
- **Actual (draft):** the offer is closed after approval, so the second
  request is ignored. The phone's answer says `certified` for another
  device id, and the draft only handles "own device id". Nobody is told.
  M's phone holds a phone-tier certificate for Ana (post, vote, answer as
  her) until someone notices and revokes it.
- **Severity:** high (impersonation at the phone tier). **Confidence:** high.

### C2. Any member can inject a request without a box (H1)

- **Claim:** the box is the return path; a stolen QR also needs a box channel.
- **Sequence:** member M with the QR's secret seals a `phone-enroll:<rid>`
  blob with the workspace key and signs it with M's own desktop key.
- **Actual (draft):** accepted (§5.5 reader rule: "a certified, unrevoked
  device of a current member"). M needs no box, no bearer token and no
  pairing secret. That widens C1 to every member.
- **Severity:** medium (it lowers C1's bar). **Confidence:** high.

### C3. A co-member's QR, and name collisions (H3)

- **Sequence:** M, a member of the same workspace, sets the display name
  "Ana" and shows Ana a QR ("scan this to set up your phone"). Ana's phone
  asks the box and shows "Add this phone to **Ana**'s devices in acme?".
  Ana confirms. M approves on M's desktop.
- **Actual (draft):** Ana's phone is certified for M's account. Ana's phone
  now acts as M, and when Ana certifies it for herself, the device has two
  human holders and `phone_person` fails closed
  (`chat_service/phone_sign.rs:89-97`): Ana's phone is unusable in that
  workspace until she resets its device key.
- **Severity:** medium (confusion and lock-out; no access to Ana's data).
  **Confidence:** high.

### C4. Filling the box's pending list (H4)

- **Sequence:** an attacker with the bearer token (or a pairing secret,
  using provisional connections) generates 8 P-256 keys and sends 8 requests
  for the real `ws` and a real desktop device id, each with a random MAC
  and `exp = now + 900`.
- **Actual (draft):** every check the box can make passes. Its 8 pending
  slots stay full for 15 minutes; it refuses honest phones; refilling every
  15 minutes keeps it so. The box also publishes 8 presence slots.
- **Severity:** medium (availability). **Confidence:** high.

### C5. No presence connection on the box (H5)

- **Sequence:** a box synced over P2P only (no relay room), phone on the
  direct path.
- **Actual (draft):** the box accepts the request (202) and can never publish
  it. The phone waits 5 minutes and sees "expired".
- **Severity:** low (stuck flow). **Confidence:** medium (depends on how
  `serve` runs without a relay).

### C6. Lost answer (H6)

- **Sequence:** the desktop certifies, then crashes or loses the relay before
  publishing its answer.
- **Actual (draft):** §9 requires both a `certified` answer *and* the box's
  `certified: true`. The certificate syncs and the box knows it, but the
  phone waits for an answer that never comes, then says "expired" even
  though it is certified.
- **Severity:** low. **Confidence:** high.

### C7. The QR on a shared screen (H7)

- **Sequence:** Ana opens Add a phone during a video call with screen share.
- **Actual (draft):** the QR is on screen for 5 minutes; anyone on the call
  has the secret (this is the setup for C1).
- **Severity:** medium (enabler). **Confidence:** high.

### C8. Late passkey failure

- **Sequence:** an account with no passkey and no recovery key opens Add a
  phone, the phone scans and waits, then the desktop's approval fails on
  presence.
- **Actual (draft):** the precheck (§8.1) doesn't cover presence, so the
  person goes through the whole dance before learning it can't work.
- **Severity:** low. **Confidence:** medium.

### C9. Two offers in flight on the phone

- **Sequence:** the phone scans desktop A's QR, then (impatient) desktop B's.
- **Actual (draft):** unspecified. Two requests may be pending; both
  desktops may certify (harmless if the same person: the second is a
  re-publish), or one gets `someone_elses` (different people). The waiting
  screen can show the wrong fingerprint context.
- **Severity:** low. **Confidence:** medium.

### C10. Secrets in logs

- **Actual (draft):** "nothing is logged" is stated for the phone only. The
  desktop's Tauri command layer and the web console log errors with their
  inputs in places (`tracing::info!` in `phones_ui.rs:108` logs the label).
  No test pins that the QR text, `s`, `k_req` or `k_ans` never reach a log.
- **Severity:** medium if it happens (a log reader gets C1's precondition).
  **Confidence:** medium.

### Checked and holding

- **Key substitution by a box or relay without the secret:** refused (MAC
  over `signed ‖ phone_sig`, which covers `code_body`). Holds.
- **Replay** of a request into another offer: `rid` and the MAC key differ.
  Within the offer: identical bytes are deduplicated. After the offer: the
  desktop holds no offer. Replay of an answer: the phone matches its own
  `rid` and device id. Holds.
- **Cross-protocol signatures:** the phone's `signed` starts with
  `ligus-enroll-request-v1\0`; the phone-action checker refuses anything not
  starting with `hive-envelope-v3\0`, and the code signature uses
  `ligus-phone-code-v1\0`. A box can't get the phone to sign one for the
  other. Holds.
- **Malleability:** a high-S `phone_sig` would be a different byte string,
  but the MAC covers it, so only a secret holder could make one. Decoders
  refuse high S anyway (the core's rule). Holds.
- **Downgrade to paste:** paste is today's flow with today's checks;
  failing the QR flow gives an attacker nothing paste didn't. Holds.
- **Wrong workspace phishing:** the box refuses `wrong_workspace`, and the
  phone matches the offer's workspace to a host profile. Holds.
- **Revoked devices:** a revoked phone is refused by the box and by
  `phone_standing`; a revoked desktop by the box's `desktop_unknown` and by
  the fold; a revoked box's slot by the reader. Holds.
- **Old clients:** an old desktop ignores the slots, an old box 404s the
  routes and answers 4040 (unknown `enrollKey` field ignored), an old phone
  never sends `enrollKey`. No relay change. Holds.
- **Provisional mode reach:** separate router, no `pid`, attempts counted,
  5 minutes. It can't reach the API router. Holds, with C4's limits.

## Required tests (round 1)

Each is executable; names are suggestions.

| # | Attack | Test |
|---|---|---|
| T1 | C1 stolen QR before the real phone, approve, then the real phone | e2e (`tunnel/e2e_tests.rs`): two simulated phones with the same offer; desktop approves the first; assert the desktop raises the "second phone" alert with the first phone's device id, and the second phone's status returns an answer whose device id ≠ its own and the phone model enters the alarm state |
| T2 | C2 member-signed slot | unit (`phone_enroll_relay.rs`): a slot sealed with the workspace key and signed by a person's desktop device is dropped; signed by a service member's device, accepted |
| T3 | C3 co-member QR | serve API: `check` returns `person.accountId`; iOS unit: a phone with a profile whose `/v1/me` account differs shows the mismatch warning; serve API: two members with the same display name → `nameCollision: true` |
| T4 | C4 pending flood | serve API property test: N callers × M requests with random MACs; honest request from a fresh channel is always accepted; per-channel cap holds; no request with `exp > now + 420` is accepted |
| T5 | C5 no relay | serve API: box without a presence connection answers 503 `relay_unavailable` |
| T6 | C6 lost answer | e2e: desktop certifies, answer publish fails (fake relay drops it); phone sees `certified: true` and succeeds |
| T7 | C7 exposure | web: QR hidden until Show code; hidden on window blur; Tauri: `set_content_protected(true)` while visible (mocked window) |
| T8 | C8 no passkey | desktop: precheck refuses before generating a secret when the account has neither passkey nor recovery key |
| T9 | C9 two offers | iOS unit: a second scan while a request is pending asks to cancel the first; only one offer in memory |
| T10 | C10 logs | Rust: run the desktop offer flow and the serve routes with a capturing `tracing` subscriber; assert neither the offer text, `s`, `k_req` nor `k_ans` (hex or b64u) appears; iOS: a debug-log sink test does the same |
| T11 | key substitution by a box | unit: request re-MAC'd with a wrong key, or code swapped, or phone_sig swapped: desktop refuses each |
| T12 | replay | unit: a valid request for offer A presented to offer B refused; same bytes twice deduplicated; answer for rid A ignored by a phone waiting on rid B |
| T13 | revoked | serve API: revoked phone → 403 `device_revoked`; revoked desktop device → 409 `desktop_unknown`; desktop: revoked relay box's slot dropped |
| T14 | old peers | tunnel: old-phone msg1 (no `enrollKey`) gets 4040; new phone vs host with E3b disabled gets 4040 and the phone falls back |

## Simpler alternatives considered

- **Box forwards every 4040 phone's code to the person's desktops** (no QR,
  no MAC). Simpler, but the box chooses which key reaches the desktop, and
  it would show requests on every desktop of every member who might own the
  phone. The MAC is what takes the box out of the trust path. Rejected.
- **Drop E3b; require the direct path or paste for an uncertified phone.**
  Much less code, but relay-only boxes (Kubernetes, a box away from home)
  are the main remote setup, and that's where the owner got stuck. Keep E3b.
- **Drop `phone_sig`.** The MAC alone gives I1. `phone_sig` adds the phone's
  live consent (a `ligusphone1:` code seen earlier can't answer an offer)
  and lets the box refuse junk early. Cheap; keep.

## Required revisions (round 1)

- **R1 (C1).** Keep listening after approval until the offer's expiry. A
  different valid request then raises a blocking alert on the desktop:
  "Another phone answered this code after you added *label*. If the phone
  you added isn't in your hand, revoke it." with a one-click **Revoke**. The
  phone, on an answer whose device id isn't its own, shows "Another phone
  was added with this code. Tell the person at the desktop to revoke it."
  The desktop also holds **Add this phone** disabled for 5 s after a request
  appears ("listening for other phones"), so a second request racing the
  first is likely to abort the offer before approval.
- **R2 (C2).** Only a device of a current **service member** (a box) may sign
  a `phone-enroll:*` slot; only the offer's own desktop device may sign a
  `phone-enroll-answer:*` slot. Put the relaying box's device id in the slot
  name (`phone-enroll:<rid>:<box device id>`).
- **R3 (C7).** The QR is hidden until **Show code**, hidden again when the
  window loses focus or the app is backgrounded, never selectable or
  copyable, and the window is content-protected while it shows
  (best effort: it keeps it out of screenshots and most screen sharing, not
  all).
- **R4 (C3).** `check` answers the person's `accountId` and a
  `nameCollision` flag. The phone shows the role and, if it already knows
  its account from another host profile, warns when the offer's account
  differs. The paste-flow lock-out (anyone who has a phone's public code can
  certify it for themselves) is pre-existing and out of scope; record it.
- **R5 (C4).** Box limits per channel: 1 pending per provisional connection,
  2 per bearer/operator caller; when the global cap (16) is full, evict the
  oldest pending request of the caller with the most; accept `exp ≤ now +
  300 + 120`.
- **R6 (C5).** Refuse with `503 relay_unavailable` (in `check` already) when
  the box has no presence connection.
- **R7 (C6).** The phone's success is the box's `certified: true` (then E3a
  or `/v1/me`). The answer is for `declined`, `refused` and the R1 alarm.
- **R8 (C8).** The desktop's precheck includes "this account can give
  presence" (a passkey, or a recovery key on a CLI identity).
- **R9 (C9).** One offer at a time on the phone; a second scan asks to
  cancel the first.
- **R10 (C10).** Log hygiene stated for all three sides, with T10.

---
# Round 2 (revised design)

## Decision

**Approve with risks.**

R1 to R10 landed as the design's §15 maps them, and each was checked
against the text. Round 2 found two small defects introduced or left by the
revision (R11, R12 below); both were fixed in revision 3 and re-checked.
Nothing left needs another round. The risks that remain are listed at the
end; none is a reason to hold the build, and each has a test that shows its
bound.

## What was re-checked

- **R1 hold and tripwire.** The hold aborts a second phone within 5 s; after
  approval the desktop watches until `exp`, the alert offers a passkey
  revocation, and the answer slot keeps naming the certified device so the
  real phone raises its alarm. The phone treats an answer naming another
  device as an alarm, not a success. Holds.
- **R2 signer narrowing.** A request slot needs a service member's device
  signature; a person's device (even holding the workspace key and the
  secret) is dropped. The answer slot needs the offer's own desktop device.
  Holds. A box operator holding a stolen secret is still possible: RR2.
- **R3 exposure.** Show code, hide on blur or background, image only,
  content protection while visible. Holds, best effort: RR4.
- **R4 co-member QR.** `accountId`, `memberId`, `nameCollision`, the role on
  the confirm sheet and the other-account warning. Mitigates; doesn't
  prevent a person who ignores both: RR5.
- **R5 flood.** Per-channel caps (provisional 1, bearer/operator 2, paired
  peer 1), a 16-entry global cap that evicts from the heaviest caller, and
  `exp ≤ now + 420`. A pairing-secret holder gets at most 5 provisional
  connections per `ligus pair` window; a bearer holder at most 2 pending. An
  honest phone on another channel always gets a slot. Holds.
- **R6 to R10.** `relay_unavailable`; success on the box's `certified:
  true` (a box that lies about it only makes the phone try E3a or `/v1/me`,
  which refuse); the presence precheck; one offer at a time; log hygiene
  with T10. Hold.

## New findings (round 2)

### C11. A restarted phone looks like "another phone" (fixed: R11)

- **Claim:** "a *different* valid request ... aborts the offer" (revision 2,
  §8.1).
- **Sequence:** the phone submits, the app is killed (memory pressure), the
  person reopens it and scans the same QR again. The Secure Enclave's ECDSA
  signatures are randomized, so the new request's bytes differ.
- **Actual (revision 2):** "different" meant different bytes: the offer
  aborts (before approval) or the tripwire fires (after), a false alarm on
  an honest retry.
- **Fix (revision 3):** "another phone" means another device id or key; the
  same phone's fresh request is the same phone. **T15:** unit: two valid
  requests from the same key with different signatures neither abort nor
  trip; a request from a second key does.

### C12. Phone clock tolerance (fixed: R12)

- **Sequence:** the phone's clock runs 3 minutes fast (manual time, travel).
  The desktop issues `exp = now + 300`.
- **Actual (revision 2):** the phone refused `exp` "in the past" with no
  tolerance, while the box allows 120 s. A phone a little off couldn't use
  fresh codes.
- **Fix (revision 3):** the phone accepts `exp` up to 120 s past, like the
  box; the desktop's own clock is what ends the offer. **T16:** iOS unit
  with an injected clock: `exp = now − 60` accepted, `now − 180` refused.

### Checked and holding (round 2)

- **Crash recovery.** Every piece of new state is in memory with a lifetime
  ≤ 7 minutes: the desktop's offer, the box's pending list, the phone's
  offer. A crash anywhere loses at most one attempt; the certificate, if it
  was written, syncs as today and E3a or `/v1/me` finds it. No migration,
  nothing to roll back.
- **Ordering.** Presence is last-writer-wins and unordered; the protocol
  never depends on order: the desktop deduplicates by device and key, the
  box by device, the phone by `rid`. A slot that arrives before the keyring
  epoch it was sealed under is retried on the next poll (the box
  re-publishes every 10 s).
- **Concurrent desktops.** Two of the person's desktops certifying the same
  phone write the same certificate; the second is a re-publish the fold
  accepts without presence (`authz/rules.rs:33-37`). Two people's desktops:
  the second gets `someone_elses` from the box, `Conflict` from
  `phone_standing`. Holds.
- **Removed member.** A person removed while their offer is open can't get
  a usable phone: the certificate may fold, but `phone_person` names no
  member (`chat_service/phone_sign.rs:77-99`) and every phone route refuses.
  Holds (the same as the paste flow).
- **Versioning.** `ligusenroll1:`, `ligusenrollreq1:`, `ligusenrollres1:`
  are versioned by prefix; E3b by an optional msg1 field and a msg2 field
  only a new phone reads. Nothing existing is renamed: `ligusphone1:`,
  `ligus-phone-code-v1\0`, `ligus-device-id-v1\0`, `hive-envelope-v3\0`,
  `hive-tunnel/1`, the UUIDv5 namespaces and `x-hive-*` headers are
  untouched. Holds.

## Required tests (round 2)

T1 to T14 from round 1, plus T15 and T16 above.

## Residual risks

- **RR1. Stolen QR, approved without reading the fingerprint, and the real
  phone never answers.** Someone who saw the QR in its 5 minutes and can
  get a box to relay (a pairing secret, the box's bearer token, or a box
  they operate) answers first, more than 5 s ahead of the real phone; the
  person approves without comparing; the real phone never scans (the person
  thinks it's done). The tripwire then never fires. Bound: the attacker's
  key is visible in My devices as a second phone, and the real phone keeps
  being refused at pairing until it's added, which sends the person back to
  the panel. Test: T1 variant with no second phone asserts the extra device
  is listed.
- **RR2. A malicious box operator with the QR's secret** can relay their own
  request and withhold the real one, so neither the hold nor the tripwire
  sees two phones. Only the fingerprint comparison stops it. Same bound as
  RR1. This is the box-in-the-path case the MAC can't help with once the
  secret is lost.
- **RR3. Slot clobbering.** The relay doesn't bind presence slot names to
  their writer (`relay/server.go:804-824`), so the relay or any member can
  overwrite `phone-enroll:*` slots with junk. Availability only: forged
  content fails the signature and MAC checks. Fall back to paste.
- **RR4. Content protection is best effort.** Some capture paths (recent
  macOS screen-capture APIs, a camera pointed at the screen) still see the
  QR. Show code, hide-on-blur and the 5-minute, one-use expiry bound it.
- **RR5. Co-member phishing.** A member who gets a person to scan their QR,
  past the name, role and other-account warnings, binds that phone to their
  own account; the phone's person is then locked out of it in that
  workspace until they reset the device key (two holders, `phone_person`
  fails closed). No access to the victim's data.
- **RR6. Metadata.** The relay sees `phone-enroll:*` slot names, a random
  `rid`, the relaying box's device id and the timing: that someone in the
  workspace is adding a phone now. No content.
- **RR7. Tunnel attempt budget.** Provisional (1, more if the person is
  slow) plus the final enrollment (1) come out of the window's 5 attempts;
  a slow or retried flow can exhaust it, and the person runs `ligus pair`
  again.
- **RR8. Pre-existing, out of scope: the paste flow's lock-out.** Anyone in
  the workspace who has a phone's `ligusphone1:` code (it is public: shown
  on the phone; owner question O1 was answered no, so it never goes through
  the clipboard) can certify it
  for their own account, leaving the phone with two holders and its person
  locked out (RR5's effect, without any scan). The reverse-QR flow is
  immune (it needs the phone's fresh signature over the offer), but paste
  stays. Recommended follow-up: let `phone_standing` warn when a code's
  phone has never been seen by this person's boxes, or require a fresh
  phone signature in paste too (a second code that names the desktop).
  **Owner decision (2026-11-20): later.** A known follow-up, not part of
  0.1.1 (design §16).

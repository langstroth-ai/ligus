# Devices & keys, and the word form of a fingerprint (design note)

Status: design only, not implemented. Target: Ligus 0.3 (desktop page) and
the iPhone app's "Devices & keys" under You. The phone half is specified in
the `ligus-ios` repository (`docs/devices-and-keys.md`); the word list and its
encoding are specified **here** and are the single source for both. Ligus is
short for *ligustica*, the Italian honeybee.

Two people (or a person and their phone) compare a fingerprint to decide which
key becomes trusted. Today that is 20 hex characters on a desktop and one long
hex line on the phone. This note gathers devices, phones, passkeys and the
safety code on one page, and adds an optional **word form** that says exactly
the same bytes in words that are easier to compare aloud.

## 1. What exists today (code-verified)

Line numbers are as of `origin/main` at `001a9b8a6`.

**Two fingerprints, both truncated SHA-256:**

| Name | Bytes | Computed by | Shown as |
|---|---|---|---|
| Device fingerprint (desktops, boxes, phones) | first 10 bytes of SHA-256(device public key) | `ligus_runtime::device_enroll::fingerprint` (`crates/ligus-runtime/src/device_enroll.rs:160-171`); phone port `DeviceFingerprint.of` (`ligus-ios: Ligus/PhoneActions/PhoneEnroll.swift:42-48`) | `7F3A-91C0-77DE-0B14-5E62`, upper-case, five groups |
| Box join-request fingerprint | first 10 bytes of SHA-256(`"ligus-box-join-v1\0"` ‖ account ‖ device ‖ keys) | `box_admission::JoinRequest::fingerprint` (`box_admission.rs:69-81`) | same format |
| Safety code (a person's account key) | first 16 bytes of SHA-256(account signing key) | `account_keys::key_fingerprint` (`crates/ligus-core/src/account_keys.rs:327`), grouped by `directory_trust::safety_code` (`crates/ligus-runtime/src/directory_trust.rs:362`) | `0011 2233 4455 …`, lower-case, eight groups |

Golden vector: key `04 01..40` → `0ED3-A6AB-957F-F6F5-9A96`
(`phone_enroll.rs:239-243`, also `docs/ios-app.md:432`).

**Desktop UI.** No shared fingerprint component; each place renders a
`<code>`: `settings/AccountSection.tsx` `SafetyCodeSection` (:97-133) and
`DevicesSection` "My devices" (:256, fingerprint at :437, Revoke at
:446-453); `settings/AddPhonePanel.tsx` (:371-377, :538-544, with
`aria-label={spellFingerprint(…)}` :459, and the checkbox "Your phone shows the
same fingerprint" :389); `AdmitBoxModal.tsx:115`; `PhoneEnrollTripwire.tsx`.
Passkeys are in `settings/SecuritySection.tsx:96-110` as
`credentialId.slice(0,16)…`; `PasskeyDto` (`app/src/passkey_ui.rs:378`)
carries only `credential_id` and `recovery`, although `PasskeyCredential`
(`crates/ligus-core/src/presence.rs:49`) has a `label` and `registered_at`.

**Revocation.** `DeviceRevoked { device_id, watermark }` (`events.rs:288`);
Admin+ with passkey presence, or any role for your own devices
(`authz/mod.rs:666-673`), never a higher-ranked member's
(`:1357-1376`). Desktop commands `list_my_devices` (`app/src/lib.rs:5114`,
`MyDeviceDto` :4999), `revoke_my_device` (:5281), `list_device_revocations`
(:5332), `rotate_workspace_key` (:5508). Confirmation copy:
`web/src/lib/myDevices.ts:38` `revokeConfirmText`.

**Word lists.** Only BIP-39 (`bip39 = "3"` in `crates/ligus-core/Cargo.toml`,
used by `RecoveryKey::words`, `presence.rs:696-708`). The iPhone app has no
word list and no Rust core: it re-implements in Swift and keeps parity with
golden vectors.

## 2. The word form

### 2.1 The list

The **PGP word list** (two lists of 256 words: "even" words for bytes at even
positions, "odd" words for odd positions). It was designed for reading
fingerprints aloud: the words are phonetically distinct, the even list is
two-syllable and the odd list three-syllable, so a swapped, dropped or
repeated word is audible.

Why not the others:

- **BIP-39** (already a dependency): 11 bits per word means 80 bits doesn't
  divide into words, words don't line up with hex groups, and the list was
  chosen for writing down, not for telling words apart by ear. Reusing the
  recovery-key list for a comparison code also invites confusing the two.
- **EFF short list:** 10.3 bits per word, same alignment problem, and an
  attribution licence.

**Owner to confirm:** the PGP word list's licence terms for redistribution in
both apps (it is widely reproduced; we need a citable statement before
shipping).

### 2.2 Encoding (normative)

`words(bytes)`: for each byte `b` at index `i` (0-based), take
`EVEN[b]` if `i` is even, else `ODD[b]`. No truncation, no checksum, no
padding. The word form says **all** the bytes the hex says.

- Device fingerprint: 10 bytes → 10 words, shown in **five pairs that line up
  with the five hex groups**:
  `7F3A-91C0-77DE-0B14-5E62` → `word word · word word · word word · word word · word word`.
- Safety code: 16 bytes → 16 words in eight pairs, lined up the same way.
- Words are shown in lower case, separated by a space inside a pair and
  ` · ` between pairs.

### 2.3 One list on every surface

- The list lives in `crates/ligus-core/src/fingerprint_words.rs` as two
  `const [&str; 256]` arrays, with `FINGERPRINT_WORDS_V1_SHA256`: the SHA-256
  of `EVEN.join("\n") + "\n" + ODD.join("\n")`. A unit test recomputes it.
- `device_enroll::fingerprint_words(key) -> [&'static str; 10]` and
  `directory_trust::safety_code_words(..)` are computed from the **same
  bytes** as the hex, in the same function family, never by parsing the
  displayed string.
- DTOs gain additive fields next to every fingerprint they carry
  (`MyDeviceDto.fingerprint_words`, the add-phone and admit-box previews,
  `/v1/me` and `/v1/profile` on `ligus serve`). No field is renamed.
- The iPhone app gets `FingerprintWords.swift`, generated from the Rust file
  by a script, and its test recomputes the same `…_SHA256` constant. The
  phone computes words from its own key bytes (as it does the hex today); it
  never takes words from the host.
- Golden vectors `tests/golden/fingerprint_words_v1.jsonl` (key hex,
  fingerprint hex, words) are copied byte for byte to
  `LigusTests/Fixtures/golden/` in `ligus-ios`; both test suites read them.
- Changing the list is a new version (`…_v2`) with both sides shipping it
  before either shows it.

### 2.4 Display rules

- The hex is always shown. Words are **under** it, when the person has turned
  on "Show as words" (a per-device preference, off by default). Never words
  alone.
- Large mono text for the hex, groups of four, never shrunk below body size;
  wraps at a group boundary.
- Accessibility: the hex keeps `spellFingerprint`'s label; the words get
  their own label ("Fingerprint words: …, …").
- Hidden while the screen is shared, wherever the hex is (on the phone,
  `.hiddenWhileScreenShared()`; on the desktop, the existing content
  protection during Add a phone).
- English only, on every locale: both sides must show identical words.

### 2.5 Exact copy (security-critical)

| Where | Text |
|---|---|
| Toggle | Show as words |
| Under a fingerprint, words on | The words say the same as the letters. Compare all five groups. |
| Add a phone, step 3 | Compare with your phone. Every group must match, letters or words. |
| Checkbox | My phone shows the same fingerprint |
| Secondary action | Not my phone |
| Help under the checkbox | If any group is different, choose Not my phone: someone may be trying to add their phone as you. |
| Admit a box | Check it matches what the box printed, all five groups. |
| Safety code | Read this to someone you're inviting, or compare it with the code they see for you. If every group matches, the invite really is from you. |
| Safety code, words on | Reading it aloud? The words say the same as the letters. |

## 3. The Devices & keys page (desktop)

Reached from Settings → You, the palette ("Devices & keys", "Add a phone")
and the Inbox nudge after the first box joins. Sections, top to bottom:

1. **This device**: name, kind, fingerprint (component above), "This device".
2. **Your other devices**: each with fingerprint, last seen, and **Revoke…**.
3. **Phones**: each with fingerprint, tier ("phone level: post, vote,
   answer; can't change members, devices or keys"), "Relayed by ‹box›", last
   seen, **Revoke…**; **Add a phone** (the 3-step sheet: Prepare, Scan,
   Confirm), unchanged in logic (`docs/design/v2/phone-enroll-reverse-qr.md`).
4. **Passkeys**: label, registered date, "Recovery key" mark, **Remove…**
   (`PasskeyDto` gains `label` and `registered_at`, additive).
5. **Safety code**, with the copy above.

Revoke confirmation keeps `revokeConfirmText` and adds one line:
"Revoking stops every member accepting its events. It doesn't take back what
it already read. To cut that too, rotate the workspace key." with a
**Rotate key…** link to the existing `rotate_workspace_key`.

A `Fingerprint` component (`web/src/components/Fingerprint.tsx`) replaces
every ad-hoc `<code>` above: props `hex`, `words?`, `label`.

## 4. Tests (executable)

| # | Test | Kind |
|---|---|---|
| T1 | Both lists have 256 unique entries, no word in both lists, lower-case ASCII only; the hash constant matches. Same test in Swift. | unit (Rust + Swift) |
| T2 | Golden vectors: the same key gives the same hex and words in Rust and Swift (shared JSONL). | golden, both repos |
| T3 | Property: `words` is injective over random 10- and 16-byte inputs, and decoding words back gives the bytes. | proptest |
| T4 | The words for a device come from the key bytes: a DTO whose `fingerprint` and `fingerprintWords` disagree can't be built (constructor test), and the web component refuses to render words if the hex has a different group count. | unit + vitest |
| T5 | Words never appear without the hex (component snapshot with words on and off). | vitest snapshot |
| T6 | Add a phone with words on: the approve call still sends the hex fingerprint that was compared (`phoneEnrollOfferApprove(…, fingerprint)`), unchanged. | vitest + existing Rust test |
| T7 | Passkeys list shows labels; Remove still requires the existing confirmation. | vitest |

## 5. Adversarial assessment

Stance: the adversarial reviewer, security lens (security-critical copy).
The first pass looked at a draft that used BIP-39, showed words *instead of*
hex when enabled, and computed words in TypeScript from the displayed string.

### First pass: Revise

1. **Two encoders drift** (High, medium). A TypeScript port and a Swift
   port of a word mapping can disagree silently; a disagreement shows as a
   mismatch (safe but trust-eroding) or, with a bad port of a
   non-injective map, a false match. *Revision: Rust is the only desktop
   encoder, a generated Swift list, a pinned list hash and shared vectors (2.3),
   tests T1-T3.*
2. **Words instead of hex** (Medium, high). A phone on an older build shows
   only hex; a desktop showing only words makes the comparison impossible and
   people tick the box anyway. *Revision: hex always, words beneath (2.4),
   test T5.*
3. **Derived from the display string** (Medium, medium). Parsing the shown
   hex lets a display bug change both forms together. *Revision: from key
   bytes (2.3), test T4.*
4. **BIP-39 alignment and confusion with the recovery words** (Low, high).
   *Revision: PGP word list (2.1).*
5. **Partial comparison** (Medium, high). People compare the first group or
   two. Grinding a key whose first two groups match costs about 2^32 work, so
   a partial check is weak in either form. *Revision: copy says "all five
   groups" everywhere (2.5). Residual risk below.*

### Second pass

#### Decision

**Approve with risks.**

#### Claimed invariants

- F1. The word form encodes exactly the bytes the hex encodes, on every
  surface, for every list version.
- F2. The word list is byte-identical on desktop, box and phone, and a
  difference fails a test in both repositories.
- F3. Words never replace the hex.
- F4. No governance or key behaviour changes: what is compared, what the
  approve call binds and who may revoke are unchanged.

#### Hidden assumptions

- People actually compare every group. The design can ask but can't force it.
- The PGP list can be redistributed (owner to confirm).

#### Counterexamples considered after revision

| Claim | State | Expected | Likely actual | Severity | Confidence |
|---|---|---|---|---|---|
| F2 | Phone on a build without words | compare hex | hex shown on both; words only on desktop | Low | High |
| F1 | A list v2 ships on desktop first | no mismatch | desktop keeps showing v1 until both support v2 (versioned) | Low | Medium |
| Partial check | Attacker grinds 32 bits | user notices groups 3-5 differ | user may stop after group 2 | Medium | Medium |

#### Required tests

T1-T7.

#### Simpler alternatives

Larger hex in groups of four, with no words. It fixes most of the
readability problem and has no list to keep in sync. The word form is still
worth it for comparison by voice (a phone call with a new member).

#### Residual risks

- Partial comparison by people (unchanged by this design, only discouraged by
  copy).
- English words on non-English devices.
</content>
</invoke>

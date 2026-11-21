# Ligus on Android: specification

Status: **design spec, not yet built.** It describes an Android client with
feature parity to the Ligus iPhone app (Ligus is short for *ligustica*; the
platform is Langstroth) as of 2026-11-20. Like the iPhone app, it is a thin
client of `ligus serve`. The app's wire contract is unchanged: it is
[`ios-app.md`](ios-app.md) (the `ligus serve` API, "Add this phone", phone
actions), [`relay-tunnel.md`](relay-tunnel.md) (`hive-tunnel/1`) and
[`push-notifications.md`](push-notifications.md). This document covers only
what is specific to Android, plus the relay change that Android push needs.

The key words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119. Strings
in `code` such as `hive-envelope-v3\0`, `hive-tunnel/1` and the push
payload's `hive` key are frozen wire identifiers. They are never renamed, and
the app uses them byte for byte.

Sources this spec was checked against:

- This repo: [`ios-app.md`](ios-app.md), [`relay-tunnel.md`](relay-tunnel.md),
  [`push-notifications.md`](push-notifications.md),
  [`serve-api.md`](../docs-site/docs/operate/reference/serve-api.md),
  [`phone-app.md`](../docs-site/docs/ligus/features/phone-app.md),
  [`threat-model.md`](../docs-site/docs/operate/security/threat-model.md),
  [`native-passkeys.md`](../docs-site/docs/operate/security/native-passkeys.md);
  the code in `crates/ligus-cli/src/serve/` (`api.rs`, `me_api.rs`,
  `phone_actions_api.rs`, `admissions_api.rs`, `memory_api.rs`, `tunnel/`),
  `crates/ligus-runtime/src/phone_enroll.rs`,
  `crates/ligus-core/src/{phone_action,presence,crypto,admission,channel}.rs`,
  `crates/ligus-core/src/authz/votes.rs` and `crates/ligus-proto/src/names.rs`;
  and the golden vectors in `crates/ligus-core/tests/golden/`,
  `crates/ligus-proto/tests/vectors/display-names.json` and
  [`relay-tunnel/vectors.json`](relay-tunnel/vectors.json).
- The relay (`langstroth-ai/langstroth-relay`): `tunnel.go` (`parseApns`,
  session admission), `tunnel_policy.go` (`PushToken`, `PushSender`) and
  `push_apns.go`.
- The iPhone app's repository (branch `feat/ligus-2.0`): its README, its
  `docs/` (`DESIGN.md`, `DEVELOPMENT.md`, `app-review.md`), its Swift
  sources and tests, and the findings of its Phase B and enrollment reviews.
  That repository is private. This spec describes its behaviour without
  copying it.

---

## 1. Goals and non-goals

### 1.1 Goals

1. **Parity.** Everything the iPhone app does, the Android app does, against
   the same `ligus serve` contract with no box changes. Section 1.3 lists
   the exceptions: push needs a relay change, passkeys need a workspace
   presence-policy entry, and a few platform features differ.
2. **The same security bar.** Every check the iPhone app makes before it
   signs, pins or shows something, the Android app makes too, with the same
   refusals (section 3). Every finding from the iPhone reviews maps to an
   Android requirement with a test.
3. **One implementation of the risky parsing.** Preimage checks, the phone
   code, device-id derivation, Noise and push decryption come from the Rust
   core through UniFFI (section 2.9), not from a third hand-written port.

### 1.2 Feature matrix

"Same" means the same contract, routes and behaviour, built with the Android
equivalent.

| iPhone feature | Android equivalent | Notes / differences |
|---|---|---|
| Pairing screen: scan a QR code, paste a link, type an address and token, Try the demo | Same. CameraX with ML Kit barcode scanning (QR only), or the ZXing core for a Play-services-free build | Accept only `ligus://` links, as iOS does |
| Pairing link v1 (`url`, `token`) and v2 (`v=2&hid&hs&relay&ps&exp`) | Same parser (Rust core) | `relay` MUST be `https` (http only in debug builds or for loopback); refuse a link with `exp < now − 300` |
| Pair confirm sheet (direct and remote) | Same | |
| Pairing refusals 4040 / 4041 / 4042 / 4001 | Same text and policy | 4040: Retry while the link lasts, at most 5 attempts. 4041 and 4042 are final. 4001 during enrollment reads "pairing code expired or already used" |
| Settings > This phone: `ligusphone1:` code as a QR code and text, plus the fingerprint | Same, under **Can't scan? Show this phone's code** (0.1.1) | The code's text isn't selectable. Copy is sensitive and cleared after 600 s (section 2.11). `FLAG_SECURE` (section 2.12) |
| Add to my devices (0.1.1): scan the desktop's `ligusenroll1:` QR, the request goes to the desktop through the box, the desktop approves with its passkey | Same (section 3.9). CameraX with ML Kit barcode scanning (QR only), in-app scanner only; ZXing core in the FOSS flavor | The `ligusphone1:` code stays under Can't scan?. Through the relay an uncertified phone uses E3b provisional enrollment (section 2.9) |
| Device key: Secure Enclave P-256, `.privateKeyUsage` | Android Keystore P-256, StrongBox when available, non-exportable, not auth-bound (section 2.3) | A software-only Keystore is refused outside debug builds |
| `GET /v1/me` after pairing; first answer pinned per device | Same (section 3.3) | |
| Host profiles (several), path preference Auto / Direct only / Relay only, 1.5 s direct probe | Same | |
| Direct path: plain HTTP to LAN or tailnet, bearer token | Same, with an app-enforced address policy (section 2.10) | iOS uses ATS exceptions. Android's network security config can't express address ranges, so the app enforces the policy in code |
| Relay tunnel client: Noise IK/IKpsk1, mux, HTTP over mux, SSE, `Last-Event-ID` resume | Same protocol. Noise, mux and the crypto are in the Rust core; OkHttp WebSocket and the HTTP mapping are in Kotlin | |
| Credential renewal (`POST /v1/tunnel/credential` when `iat` is more than 24 h old) | Same | |
| Direct-token rotation (`GET /v1/tunnel/direct` after a direct-path 401) | Same | |
| Tabs: Chats, Agents, Review, Dashboard (hidden when empty), Settings | Same, as a Compose `NavigationBar` (phones) or `NavigationRail` with list-detail panes (tablets and foldables) | iPad keyboard shortcuts map to hardware-keyboard shortcuts on tablets |
| Chats: list (DMs first), chat view, composer, Markdown (GFM tables, task lists), streaming turns, hand-off cards, ask cards, lockdown bar with Clear, founder-wait note, typing row | Same | Markdown links open only `http`, `https` and `mailto` |
| Repo chip, repo picker (all code hosts), clone or clone via fork, pull, draft PR/MR sheet | Same | Plain bearer API calls, not signed |
| Chat details, DM person view | Same | |
| "Propose as a memory note" (`proposeMemory`) | Same | |
| Agents tab with host badges, and whether you may use each agent | Same (read-only) | Moving an agent is refused for a box; leave it out, as the desktop does. Grey out an agent with `available: false` and show its `unavailableReason`; `access` is `owner` or `everyone` (`docs/ios-app.md`, `GET /v1/agents`) |
| Review: list, swipe to approve or request changes, proposal detail, votes, discussion, request changes, diff, rendered Markdown (Diff / Rendered, Before / After), Implement-plan card, Apply on ‹host› / Withdraw, proposal sets, dearth banner, Flow | Same | A swipe never signs on its own: it opens the review sheet |
| Dashboard panels (stat, gauge, timeseries, table, rows, text, status), lab fallback | Same | |
| People and safety codes, look-alike badges, link chips, invites (inspect, accept, dismiss), DMs | Same | Invite calls go through the tunnel whenever the host has remote access |
| Admit a box (`ligusjoin1:` request, passkey, `ligusadmit1:` or bearer code) | Same | Bearer admission code: sensitive copy cleared after 120 s, never shared. `FLAG_SECURE` |
| Code hosts settings (forge connections, token entry over the tunnel only, device-flow sign-in) | Same | |
| Appearance ("Match host": `pollen`, `studio`, `harbor`, `slate`, `meadow`, `obsidian`; `auto`/`light`/`dark`) | Same palettes as Material 3 color schemes | Dynamic color (Material You) is an opt-in palette, off for "Match host" |
| Privacy: "Share typing status" | Same | |
| Passkey presence (ASAuthorization, platform and security-key providers) | Credential Manager `GetPublicKeyCredentialOption` (section 2.4) | Needs `assetlinks.json` and an Android origin in the workspace presence policy |
| Phone actions sheet ("What this phone read") | Same (section 3.1) | |
| Signed-lamport ledger | Same, failing closed (section 3.2) | |
| Face ID / Touch ID app lock, opaque privacy cover | BiometricPrompt (`BIOMETRIC_STRONG or DEVICE_CREDENTIAL`), lock overlay, recents screenshot disabled (section 2.5) | |
| APNs push, decrypted by a Notification Service Extension | FCM data message decrypted in the app's `FirebaseMessagingService` (section 2.6). UnifiedPush later | **Needs a relay change** (section 2.6.3) |
| Notification actions Reply and Approve, never signing on their own | Same, through `Activity` PendingIntents with `setAuthenticationRequired(true)` (section 3.5) | |
| `ligus://pair`, `ligus://chat/<id>`, `ligus://proposal/<id>` | Same scheme (section 2.7) | iOS has no universal links, so neither does Android v1 |
| Revocation (host GOAWAY `revoked` vs relay 4003) | Same semantics (section 3.4) | |
| Demo mode (in-process mock; on-device model or scripted replies) | Same mock, with scripted replies | An on-device model is out of scope for v1 |
| iPad split view | Adaptive list-detail layouts | |

### 1.3 Non-goals for v1

- **No new server capabilities.** Anything the phone tier can't do today
  (moving an agent, inviting a person, DMs as the person, switching dearth
  mode, applying a proposal-set alternative) stays on the desktop, as on
  iOS ([phone-app.md, "What stays on the desktop"](../docs-site/docs/ligus/features/phone-app.md#what-stays-on-the-desktop)).
- **No https App Links** for chats or proposals, and no web fallback (section 2.7).
- **No on-device model in the demo.** The demo uses scripted replies.
- **No Wear OS, Android Auto or home-screen widgets.**
- **No key attestation sent to the desktop** at certification (an open
  question, section 8).
- **No Play-services-free push in M3.** UnifiedPush comes after FCM (section 2.6).
- **No phone-side passkey registration.** As on iOS, the phone uses passkeys
  the person already has (synced through Google Password Manager or another
  provider, or a security key).

---

## 2. Platform choices

### 2.1 Language and UI: Kotlin and Jetpack Compose

**Recommendation: native Kotlin with Jetpack Compose (Material 3),
coroutines and Flow, with the protocol core in Rust through UniFFI
(section 2.9).**

- **Flutter** would put a Dart layer between the app and every API this app
  depends on: the Keystore with StrongBox, BiometricPrompt, Credential
  Manager, `FLAG_SECURE`, sensitive clips and notification trampolines. Each
  would go through platform channels that need their own audit. The iOS app
  is already native SwiftUI, so Flutter wouldn't give one shared UI codebase
  either.
- **Kotlin Multiplatform** shares Kotlin with iOS. But the iOS app is a
  mature Swift 6 codebase, and the code worth sharing (the preimage checks,
  the crypto and the tunnel) already exists, tested, in Rust. Sharing it
  through Rust serves the desktop, the box, iOS (later, if adopted) and
  Android from one source.
- **Compose** matches SwiftUI's declarative model, so screens port one for
  one, and adaptive list-detail layouts give the iPad split view.

Structure: one Gradle project with the modules `app` (UI, navigation),
`core-api` (the serve API models and client), `transport` (the direct and
tunnel transports, behind one `LigusTransport` interface as on iOS),
`security` (Keystore, the ledger, biometric, clipboard), `push`
(FCM, and later UnifiedPush) and `rustcore` (UniFFI bindings and the native
libraries). Dependency injection is manual or Hilt. Persistence uses
DataStore for preferences, plus encrypted files for secrets (section 2.3.4).

### 2.2 Minimum SDK

**Recommendation: `minSdk 30` (Android 11); `targetSdk` and `compileSdk` the
current stable API level (36 at the time of writing).**

What API 30 guarantees:

- `KeyGenParameterSpec.Builder.setUserAuthenticationParameters(timeout,
  AUTH_BIOMETRIC_STRONG | AUTH_DEVICE_CREDENTIAL)`, and BiometricPrompt with
  `BIOMETRIC_STRONG or DEVICE_CREDENTIAL` combined. That's the
  `LAPolicy.deviceOwnerAuthentication` equivalent: biometrics with the
  device credential as fallback.
- StrongBox (API 28+) and `setUnlockedDeviceRequired` (API 28+).
- Credential Manager passkeys (API 28+ through Play services).
- Scoped storage everywhere, and package visibility rules (no
  `QUERY_ALL_PACKAGES`).

What it leaves to version checks:

- the sensitive-clip flag `ClipDescription.EXTRA_IS_SENSITIVE`, a constant
  from API 33 (on 30–32 set the literal key `android.content.extra.IS_SENSITIVE`);
- `setRecentsScreenshotEnabled(false)` (API 33);
- `KeyInfo.getSecurityLevel()` (API 31; `isInsideSecureHardware()` on 30);
- `setHideOverlayWindows(true)` (API 31);
- `PendingIntent` / notification `Action.setAuthenticationRequired` (API 31);
- the `POST_NOTIFICATIONS` runtime permission (API 33).

API 28 would add Android 9 and 10 devices but lose the combined
biometric-or-credential prompt and auth parameters. The iOS app requires
iOS 26, so a fairly recent floor is consistent.

### 2.3 Key storage and the device key

#### 2.3.1 The device key (workspace P-256 key)

On iOS the device key is a Secure Enclave
`SecureEnclave.P256.Signing.PrivateKey`. Its access control is
`kSecAttrAccessibleWhenUnlockedThisDeviceOnly` with `.privateKeyUsage` only.
There is **no** `.userPresence` and no biometry: Face ID doesn't gate signing,
and the app lock only hides the UI.

Android equivalent:

```kotlin
KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
    .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
    .setDigests(KeyProperties.DIGEST_SHA256)
    .setIsStrongBoxBacked(true)          // retry without on StrongBoxUnavailableException
    .setUnlockedDeviceRequired(true)     // ≈ WhenUnlocked
    .setUserAuthenticationRequired(false) // ≈ .privateKeyUsage (no per-use prompt)
    .build()
```

- **Non-exportable** by construction (`AndroidKeyStore`). The alias is fixed
  (`ligus.device.v1`). The app stores nothing but the alias and the public key.
- **Hardware only.** After generating the key, read `KeyInfo`. On API 31+,
  `securityLevel` MUST be `SECURITY_LEVEL_STRONGBOX` or
  `SECURITY_LEVEL_TRUSTED_ENVIRONMENT`; on API 30, `isInsideSecureHardware`
  MUST be true. Otherwise delete the key and refuse with the Android form of
  iOS's `noSecureEnclave`. Debug builds MAY allow a software key on the
  emulator, as the iOS simulator does. Record StrongBox or TEE in the
  profile and show it on **This phone**.
- **Never silently replaced.** If the alias exists but won't load (a Keystore
  error, or a key invalidated after a lock-screen change), show an error.
  Leave the alias alone and offer **Reset device key** explicitly. The iOS
  fix for an unrestorable key is the model here. A new key is a new device
  with a new id, certified again.
- **Backups.** `android:allowBackup="false"` and `dataExtractionRules`
  excluding everything, the equivalent of `ThisDeviceOnly`. Keystore keys
  never leave the device anyway.

**Why not auth-bound?** iOS's `.userPresence` corresponds to
`setUserAuthenticationRequired(true)` with
`setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG |
AUTH_DEVICE_CREDENTIAL)`, which needs a `BiometricPrompt.CryptoObject`
prompt for every signature. Three things argue against it:

- **Parity.** iOS doesn't do it.
- **It's mostly redundant.** High-value actions (votes on gated proposals,
  `clearLockdown`, admissions) already need the person's passkey, which runs
  user verification.
- **It's costly to undo.** An auth-bound key is permanently invalidated when
  the lock screen changes or biometrics are re-enrolled (unless
  `setInvalidatedByBiometricEnrollment(false)`), and each invalidation forces
  re-certification on the desktop. The auth requirement is fixed at key
  creation, so changing it later means a new device id.

Making it a per-install opt-in at key creation is an open question
(section 8).

#### 2.3.2 Signing

The Keystore `Signature.getInstance("SHA256withECDSA")` returns a **DER**
`ECDSA-Sig-Value`. The wire wants **64 bytes `r ‖ s`, low-S**
([`ios-app.md`](ios-app.md), "Phone actions" step 4;
`crates/ligus-core/src/phone_action.rs`; the phone code in
`crates/ligus-runtime/src/phone_enroll.rs`). The app MUST:

1. strictly parse the DER: a single SEQUENCE of two positive INTEGERs, minimal
   encoding, nothing trailing;
2. left-pad r and s to 32 bytes each;
3. if `s > n/2`, set `s := n − s`, where
   n = `FFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551`.
   The Keystore doesn't produce low-S signatures;
4. verify the result against the public key before sending it. This catches
   a broken conversion, not a forgery.

The conversion lives in the Rust core (`der_to_raw_low_s`). The Keystore
only ever signs bytes that the core has already approved (section 3.1).

The same key signs exactly three things:

| What | Bytes |
|---|---|
| Phone code | `"ligus-phone-code-v1\0" ‖ device_id(16) ‖ pubkey(65) ‖ label` (`phone_enroll::preimage`) |
| Pairing device claim (E3a) | `ctx("device-bind", hid, phone_static)` = `"hive-tunnel/1 device-bind" ‖ 0x00 ‖ hid ‖ phone_static` ([`relay-tunnel.md`](relay-tunnel.md) §7.2) |
| Phone actions and box admissions | the preimage the box drafted, after the core's check passes (section 3.1) |

#### 2.3.3 Device id derivation

The device id is derived from the key, never chosen. It is computed by the
Rust core (`ligus_core::crypto::device_id_for(SigAlg::P256, pubkey)`):

```
pubkey    = 65-byte SEC1 uncompressed point 0x04 ‖ X ‖ Y
            (from the Keystore's X.509 SubjectPublicKeyInfo: take the BIT STRING, check 65 bytes and 0x04)
d         = SHA-256("ligus-device-id-v1\0" ‖ "p256" ‖ 0x00 ‖ pubkey)[0..16]
d[6]      = (d[6] & 0x0f) | 0x80     (UUID version 8)
d[8]      = (d[8] & 0x3f) | 0x80     (RFC 9562 variant)
device_id = lowercase hyphenated UUID of d
```

Use the 65-byte form only: never the compressed point, never the
SubjectPublicKeyInfo DER, never `"ed25519"` as the algorithm. The app's
tests MUST derive every line of `crates/ligus-core/tests/golden/device_id_v1.jsonl`.
For example, `p256-phone-actions-key` (`040217e6…6e5794`) derives
`320994f1-40c1-8502-84e5-5c781e33bb60`, and `p256-cryptokit-key` derives
`a78960d5-25cc-8dbd-8d5d-7262a0490e04`. The fingerprint is the upper-case hex
of `SHA-256(pubkey)[0..10]` as `XXXX-XXXX-XXXX-XXXX-XXXX`. Golden: the key
`04 01 02 … 40` gives `0ED3-A6AB-957F-F6F5-9A96`.

#### 2.3.4 Other secrets

| Secret | iOS | Android |
|---|---|---|
| Tunnel phone auth key (Ed25519) and Noise static (X25519) | raw bytes in the Keychain, `AfterFirstUnlockThisDeviceOnly` | generated by the Rust core from the OS CSPRNG and stored in an app-private file, sealed with AES-256-GCM under a Keystore key (`ligus.wrap.v1`, no auth, no `UnlockedDeviceRequired`). It lives in credential-encrypted storage, so it is available after the first unlock, like `AfterFirstUnlock`. Keystore Ed25519/X25519 support varies by device, so these keys are deliberately software keys, as on iOS |
| Push key per `(hid, pid)` and the last `seq` | shared Keychain group with the NSE | the same sealed store. FCM delivers to the app process, so no sharing is needed |
| Direct bearer token, `hpc1` credential, `pid` | Keychain per profile | sealed store per profile |
| Signed-lamport ledger | Keychain item `phone.lamports` | sealed file `lamports.v1` (section 3.2) |
| Pinned `/v1/me` answer (`phoneKnown`) | in the profile | in the sealed profile |

Writes are atomic: write a temp file, `fsync`, then rename. A file that won't
decrypt or parse is an **error**, never "empty" (section 3.2).

### 2.4 Passkeys: Credential Manager

The phone runs a WebAuthn **get** ceremony over the box's `challenge`. It
uses the 32 bytes decoded from the base64url string, never the string's
UTF-8. It uses the RP from `/v1/me` (`rpId`, `origins`) and the
`allowCredentials` from the prepare `preview`, with user verification
required. It never registers passkeys. Add to my devices (section 3.9)
adds nothing here: the passkey that certifies the phone is the desktop's.

- **API:** `androidx.credentials.CredentialManager.getCredential` with a
  `GetPublicKeyCredentialOption(requestJson)`. The `requestJson` is
  WebAuthn's `PublicKeyCredentialRequestOptionsJSON`: `challenge` (b64url),
  `rpId`, `allowCredentials`, `userVerification: "required"`, and a
  `timeout`. The response JSON carries `rawId`, `authenticatorData`,
  `clientDataJSON` and `signature`. Send them as
  `{credentialId, authenticatorData, clientDataJSON, signature}`, base64url
  without padding. Send `clientDataJSON` exactly as returned, never rebuilt.
- **Local checks before `/presence`** (as on iOS):
  - `authenticatorData` is at least 37 bytes;
  - `rpIdHash == SHA-256(rpId)`;
  - the UP (0x01) and UV (0x04) flags are set;
  - `clientDataJSON.type == "webauthn.get"`;
  - its `challenge` round-trips to the 32 bytes;
  - `crossOrigin` isn't `true`;
  - `origin` is in the pinned `origins`.

  The core's verifier (`presence::verify_get`) does the same checks in the
  fold.
- **The origin differs from iOS.** On iOS the platform writes
  `origin: "https://<rpId>"`. A native Android app's `clientDataJSON` carries
  **`android:apk-key-hash:<b64url(SHA-256 of the app's signing certificate)>`**.
  The core already accepts this form in a presence policy
  (`PresencePolicy::well_formed` in `crates/ligus-core/src/presence.rs`
  allows `android:apk-key-hash:` origins next to `https://` ones, at most
  8). But the default policy is only `["https://ligus.ai"]`
  (`DEFAULT_ORIGINS`). **A workspace whose policy doesn't list the app's
  Android origin refuses every assertion the Android app produces.**
  Options:
  1. *Recommended:* the workspace's presence policy (`WorkspaceRulesSet.presence`,
     on the config log, Owner only) lists the Android origin of each
     signing certificate in use: Play, plus the direct APK if it is signed by
     a different key (section 5.2). New workspaces write it at genesis. Existing
     workspaces need the Owner to append a `WorkspaceRulesSet` that adds
     it. The desktop needs a small affordance for that, which is a
     **desktop change** in M2. The app reads `origins` from `/v1/me`. If
     its own origin isn't there, it says so before starting a ceremony
     ("This workspace doesn't accept this app's passkeys yet: the workspace
     owner adds it on the desktop") instead of failing after the prompt.
  2. *Fallback:* a Custom Tab to the workspace's `/presence` hand-off page
     (origin `https://<rp>`), as the desktop does on Linux. The result has
     to return to the app, and that return path doesn't exist for phones
     today. It would be a box or site change, so this is not v1.
- **Digital Asset Links:** Credential Manager lets an app use an RP only if
  `https://<rpId>/.well-known/assetlinks.json` lists the app with
  `delegate_permission/common.get_login_creds`. It is the Android
  counterpart of the AASA `webcredentials` entry. ligus.ai serves its AASA
  from the site repository's `sites/ligus/.well-known/`; add next to it:

  ```json
  [{
    "relation": ["delegate_permission/common.get_login_creds"],
    "target": {
      "namespace": "android_app",
      "package_name": "<package name>",
      "sha256_cert_fingerprints": ["<AA:BB:… of each signing cert>"]
    }
  }]
  ```

  Serve it as `application/json` with no redirect, through the same
  `_headers` and `_redirects` rules as the AASA. A self-hosted RP (the
  workspace's own domain) needs the same file on that domain, and the
  Android origin in its policy. That mirrors the iOS and macOS rule that a
  self-hosted RP needs its own association.
- **RP pinning:** a draft whose `preview.rpId` or `origins` differ from the
  pinned `/v1/me` answer is refused before any ceremony
  (`workspacePolicy`, as on iOS).
- **Without Play services:** Credential Manager passkeys need Google Play
  services, or, on Android 14+, a third-party credential provider. A
  de-Googled device without a provider can still use the app, but can't
  perform presence actions. Say so on the sheet.

### 2.5 Biometric app lock

The lock is BiometricPrompt with
`setAllowedAuthenticators(BIOMETRIC_STRONG or DEVICE_CREDENTIAL)`, the
counterpart of `LAPolicy.deviceOwnerAuthentication`. It matches iOS:

- Off by default. It is offered once after the first pairing, and turning it
  on takes one successful authentication. Grace periods: immediately,
  1 minute, 5 minutes.
- A cold launch starts locked. Going to the background (`ON_STOP` from
  `ProcessLifecycleOwner`) records the time, and returning after the grace
  period locks. A system dialog over the app (`ON_PAUSE` without `ON_STOP`,
  such as the biometric prompt or the notification shade) doesn't lock.
  There is one automatic prompt per foreground, then an **Unlock** button.
- **Pairing never unlocks** (iOS finding IOS-4). The demo and the unpaired
  pairing screen are exempt.
- Networking and the SSE stream keep running while locked. The lock
  protects the UI on an unlocked phone; it doesn't replace revocation.
- **Privacy cover:** while the lock is on, call
  `Activity.setRecentsScreenshotEnabled(false)` (API 33+), or set
  `FLAG_SECURE` on the window (API 30–32), so the recents thumbnail shows no
  content. Draw an opaque cover (not a blur) above all content, dialogs and
  bottom sheets while the activity isn't resumed.
- A device that can no longer authenticate (no credential set) unlocks, as
  on iOS. Test what removing the lock screen does to the
  `setUnlockedDeviceRequired` device key on real devices: if the key
  becomes unusable, the app reports it (section 2.3.1) rather than replacing it.
- Notification actions while locked: section 3.5.

### 2.6 Push

#### 2.6.1 Today

The relay pushes only through APNs. In `tunnel_policy.go`, `PushToken` is
`{token (hex), env: production|development, topic?}`. `parseApns` in
`tunnel.go` accepts only the `apns` member of the phone's `auth` message,
with a hex token of at most 512 characters. The only `PushSender` is
`APNsPushSender` (`push_apns.go`). An FCM registration token isn't hex, so
the Android app can't register one at all.

Without relay changes, two things already work on Android:

- **ntfy.** `ligus serve`'s ntfy notifier sends content-free notifications
  with `Click: ligus://chat/<chatId>`. The ntfy Android app opens that link
  in the Ligus app. This needs no relay change and is the M1/M2 fallback.
- **The app's own foreground stream.** While the app is open it follows
  `/v1/events` and posts nothing.

#### 2.6.2 Recommendation

**Recommendation: FCM first (M3) for the Play build. UnifiedPush second
(M4 or later) for an F-Droid or Play-services-free build. Keep ntfy as the
no-relay fallback.**

- **FCM** is the only push channel that reliably wakes an app under Doze on
  stock Android, and it's what Play users have. Its payload is opaque to
  Google here: the relay forwards the same `ct` it forwards to APNs.
- **UnifiedPush** (a distributor such as ntfy delivering to the app) fits
  self-hosting and F-Droid. Its relay side is harder:
  - The relay would POST to an arbitrary, phone-chosen endpoint URL, so it
    needs SSRF defences: `https` only, public addresses checked at each
    connect, no redirects, a size cap.
  - Current UnifiedPush expects RFC 8291 Web Push encryption with VAPID, so
    the phone would register its `p256dh` and `auth` keys, and the relay
    would wrap `ct` a second time.

#### 2.6.3 Relay change: a push provider for FCM (spec, high level)

**This is a required relay change** (`langstroth-ai/langstroth-relay`). The
host side (`ligus serve`) is unchanged: it already sends `{"t":"push","pid","ct","prio","collapse"}`
per phone and handles the acks ([`push-notifications.md`](push-notifications.md)).

1. **Registration (phone → relay, session admission §6.4.1).** Add an
   optional `push` member to `auth`, next to `apns`, which is unchanged for
   iOS:

   ```json
   "push": {"provider": "fcm", "token": "<FCM registration token>", "app": "<package name>"}
   ```

   `null` clears it, and absent leaves it as it is. This matches `apns`.
   Validation: `provider` ∈ {`fcm`} (and later `unifiedpush`), `token` 1–4096
   printable ASCII, `app` a package name on the relay's allow-list. A phone
   sends either `apns` or `push`, never both. The relay stores the last one
   received per `(hid, pid)`. Unknown members stay ignored, so old relays
   keep working (they just never push to Android).
2. **Store.** `PushToken` gains `Provider string` (default `"apns"` for
   existing rows) and `App string`. The memory, snapshot and Postgres
   backends get the column. Backups include it.
3. **Sender.** Add an `FCMPushSender` that implements `PushSender` and
   `DetailedPushSender`. A dispatcher picks APNs or FCM by
   `token.Provider`. It uses the FCM HTTP v1 API
   (`POST https://fcm.googleapis.com/v1/projects/<project>/messages:send`,
   OAuth 2 with a service account):

   ```json
   {"message": {
     "token": "<registration token>",
     "data": {"v": "1", "hid": "<host_id>", "pid": "<pid>", "ct": "<b64u ct>"},
     "android": {"priority": "HIGH" | "NORMAL", "ttl": "3600s", "collapse_key": "<collapse>"}
   }}
   ```

   It sends a **data-only** message: no `notification` block, so Google
   never sees or shows a title, and the app always decides what to show.
   `prio` maps `high`→`HIGH` and `normal`→`NORMAL`. `ttl` matches APNs'
   1-hour expiration. `collapse` is passed as given (hosts already HMAC it).
   `ct` is at most 2,048 raw bytes, about 2.7 KB as base64url, inside FCM's
   4 KB data limit.
4. **Errors.** Map FCM `UNREGISTERED` / 404 to `ErrPushTokenGone` (the token
   is deleted, and the host is acked `no_push_token` next time), quota and
   429 errors to `rate_limited`, and anything else to `push_failed`. The ack
   codes the host knows don't change.
5. **Configuration** (open build, env; both prefixes accepted as for APNs):
   `LANGSTROTH_RELAY_FCM_PROJECT_ID`, `LANGSTROTH_RELAY_FCM_CREDENTIALS_FILE`
   (a service-account JSON, mode 0600), and `LANGSTROTH_RELAY_FCM_APPS`
   (allow-listed package names). As with APNs, a relay can push only to
   builds of the app registered in its own Firebase project. Someone
   self-hosting a relay builds the app with their own `google-services.json`.
   That's the same limitation [`push-notifications.md`](push-notifications.md#limitation-other-peoples-setups)
   describes for APNs.
6. **Discovery.** `GET /v1/tunnel/info` adds `"pushProviders": ["apns","fcm"]`,
   and the host's `welcome` keeps `push: bool` (true when any sender is
   configured). The app offers push only when its own provider is listed;
   `push: true` alone isn't enough.
7. **Revocation and deregistration** delete the token whatever its
   provider (§13.2).
8. **Tests.** Mirror the APNs tests: parse `push` (valid, null, oversize,
   both `apns` and `push`, unknown provider, app not allow-listed), the
   store round trip, a fake FCM endpoint (priority, TTL, collapse, data
   only, no `notification` key), the token-gone path, and T-push through
   the FCM sender.

**Payload encryption parity.** The phone decrypts exactly what the iOS NSE
does ([`relay-tunnel.md`](relay-tunnel.md) §14):
`ct = nonce(12) ‖ ChaCha20-Poly1305(push_key, nonce, plaintext, ad)`, with
`ad = ctx("push", host_id, pid)` and `push_key = HKDF(h_enroll, "push-key", 32)`.
It accepts the notification only if `|ts − now| ≤ 3600` and `seq` is
strictly greater than the last `seq` stored for `(hid, pid)`. On any
failure (decrypt, parse, `ts`, `seq`, an unknown `hid`/`pid`) it shows only
the generic text ("Ligus" / "New activity on your host"), with no actions
and no deep link: iOS finding IOS-1, relay-forged notifications.

#### 2.6.4 App side

- `FirebaseMessagingService.onMessageReceived`:
  1. look up the push key by `(hid, pid)`;
  2. decrypt in the Rust core;
  3. check `ts` and `seq`, and record `seq` atomically **before** posting;
  4. build the notification.

  Every high-priority message MUST post a notification, generic if
  needed. FCM deprioritizes apps that receive high-priority messages and
  show nothing.
- Content:
  - `title` and `sender` are cleaned (`ligus_proto::names` rules).
  - `body` is used only when `preview: true`. Markdown is flattened as on
    iOS and cut at 240 characters.
  - The deep link is `ligus://proposal/<id>` for `proposal.created`,
    `workflow.approval` and `review.recruit`, otherwise
    `ligus://chat/<chatId>`. Ids are validated as in section 3.6.
  - Messages from the same chat are grouped into one notification group.
- **Notification channels** (the user controls them in system settings):
  replies, mentions, reviews and approvals, pairing, plus a "generic"
  channel for undecryptable pushes. Section 7.4 sets the channels' names, importance and
  defaults. Request `POST_NOTIFICATIONS` after
  pairing through a relay that lists `fcm`, following the iOS order (after
  the lock offer). Then send `PUT /v1/tunnel/push {"enabled", "previews"}`.
- **Foreground suppression:** keep `/v1/events` open through the tunnel only
  while the app is in the foreground, and PING after 25 s of silence. On
  `ON_STOP`, close the session cleanly (RESET the stream, GOAWAY, then
  WebSocket close) so the host starts pushing. This is the host's 40 s
  foreground rule.
- **Token refresh:** `onNewToken` reconnects the tunnel session so the next
  `auth` carries the new token. When no paired relay host is left, delete
  the FCM token.

### 2.7 Deep links

- **Custom scheme.** One `intent-filter` (`VIEW`, `BROWSABLE`, `DEFAULT`,
  `scheme="ligus"`) on a single routing activity, which handles:
  - `ligus://pair?…`, which opens the pair confirm sheet (never pairs
    without a tap);
  - `ligus://chat/<id>`;
  - `ligus://proposal/<id>`.

  An unknown link shows fixed text and never echoes the URL (IOS-7).
- **No link for Add to my devices.** The desktop's `ligusenroll1:` code
  isn't a URL and no `ligus://` link starts enrollment: only the in-app
  scanner opened from Add to my devices reads it, so another app or a web
  page can't start the flow with a code it made.
- **Ids** are validated before use (section 3.6).
- **No https App Links in v1.** iOS has no universal links, and an
  `autoVerify` https filter would need `handle_all_urls` in `assetlinks.json`
  plus web fallbacks.
- **Pairing links carry secrets** (`token`, `ps`). On Android any app can
  declare the `ligus` scheme, so a link opened from another app could go to
  an impostor through the chooser. Mitigations:
  - the in-app QR scanner is the primary path, since it never leaves the
    app;
  - a pasted link is parsed in-app;
  - `ps` is single-use and bound to the first static (E2), so a stolen
    link that is used first makes the real pairing fail visibly.

  A verified https pairing link (secrets in the fragment) is an open
  question (section 8) and would need `ligus pair` to print it.

### 2.8 Notifications that act

Section 3.5 covers this: Reply and Approve use `Activity` PendingIntents,
never a `BroadcastReceiver` that signs.

### 2.9 Networking and the shared Rust core

The protocol is unchanged: `hive-tunnel/1` with WebSocket subprotocol
`hive-tunnel.1`; a Noise IK session (IKpsk1 for enrollment) with prologue
`ctx("noise", host_id)`; mux frames
`type u8 ‖ stream_id u32 BE ‖ payload`; and HTTP heads as JSON
([`relay-tunnel.md`](relay-tunnel.md) §6–§10). The timeouts, backoff and
close-code handling are as in §10 and §12. 4001, 4002, 4003, 4004, 4006 and
4026 are terminal for the phone.

**Recommendation: a shared Rust core through UniFFI, plus Kotlin for I/O and
UI.** Reimplementing in Kotlin is not recommended.

Why:

- **The iOS port shows what reimplementation costs.** The Swift app ported
  the preimage parser and needed a differential fuzz: 91k mutated golden
  preimages and raw JSON run against the Rust core. It found three places
  where the phone read bytes differently from the box:
  - UUID spellings that `Uuid::parse_str` refuses were accepted, and an
    upper-case `urn:` prefix was refused;
  - an f64 overflow read as infinity where serde_json refuses it;
  - 128 levels of nesting were read where serde_json stops at 127.

  Each was a place where the phone could approve bytes the fold reads
  differently. Every edge was then ported and pinned in a 240-case parity
  corpus, and keeping two hand-written ports in step for every new event
  field would cost that again.
- **The core is portable.** `ligus-core` depends only on serde, uuid, time,
  ed25519-dalek, x25519-dalek, chacha20poly1305, hkdf, sha2, p256, argon2,
  base64, bip39 and zeroize (`crates/ligus-core/Cargo.toml`). These are pure
  Rust crates with no system libraries, so it cross-compiles to
  `aarch64-linux-android` and `x86_64-linux-android` with `cargo-ndk`.
- **One parser everywhere.** The phone runs `check_phone_action_preimage`,
  the code every fold and the box use, so a new event field can't be read
  differently on the phone.

The binding is a new crate, `crates/ligus-mobile` (UniFFI, `cdylib`), with a
small, byte-oriented surface:

| Function | Wraps |
|---|---|
| `device_id_for_p256(pubkey) -> String`, `fingerprint(pubkey)` | `ligus_core::crypto::device_id_for`, `device_enroll::fingerprint` |
| `phone_code_preimage(id, pubkey, label)`, `phone_code_encode(…, sig)`, `phone_code_decode(text)`, `clamp_label(s)` | `ligus_runtime::phone_enroll` (or a copy of it moved into core) |
| `der_to_raw_low_s(der, pubkey) -> [u8;64]` | new; P-256 from `p256` |
| `check_phone_action(preimage, confirmed?, workspace, phone, account, action_json) -> Checked` | `phone_action::check_phone_action_preimage` |
| `check_admission(preimage, challenge, workspace, phone, account, request_device_key?) -> Terms` | `admission::check_phone_admission_preimage` |
| `presence_challenge_of(preimage)`, `check_assertion_locally(…)` | `presence` |
| `proposal_digest(parts…)` | `authz::votes::diff_digest` (including the ninth `attachment` part when present) |
| `workspace_config_session_id(ws)` | `channel::workspace_config_session_id` |
| `clean_display_name(s)` | `ligus_proto::names` |
| `parse_pair_link(url)`, `hkdf_pairing(ps)`, `parse_hpc1(cred)` | tunnel helpers (move from `ligus-cli/src/serve/tunnel` into a shared module) |
| `NoiseInitiator` (IK, IKpsk1) with the all-zero DH check, `NoiseTransport` | `snow` with the checked resolver, as on the host |
| `mux_encode/decode`, a flow-control state machine | the host's mux codec |
| `push_open(push_key, hid, pid, ct) -> Plaintext` | ChaCha20-Poly1305 per §14 |

Kotlin keeps:

- the OkHttp WebSocket and the reconnect and backoff logic;
- mapping requests onto mux streams, the SSE parser and `Last-Event-ID`
  resume;
- the Keystore, Credential Manager, BiometricPrompt, FCM and the UI.

Secrets cross the boundary as byte arrays and are zeroized on the Rust side.

Costs:

- one native library per ABI (`arm64-v8a`, `x86_64`), a few MB each after
  LTO and `strip`;
- the NDK in CI;
- the F-Droid build has to build Rust itself, which it supports.

The iOS app could adopt the same crate later through UniFFI's Swift
bindings, an open question in section 8.

**Add to my devices (0.1.1).** The surface gains `ligus_runtime::phone_enroll_qr`'s
phone side: `PhoneOffer::scan(text, now_ms)`, `fields()`, `build_request(code,
sign)` and `check_answer(text, my_device) -> PhoneVerdict`, so the offer,
request and answer are parsed by the core. The tunnel client gains **E3b,
provisional enrollment**: msg1 carries `enrollKey` (the phone's 65-byte SEC1
P-256 key, base64url) next to `device`; where the host would close with
`4040 device_not_certified`, a 0.1.1 host answers msg2
`{"v":1,"provisional":true}` (no `pid`, no `cred`), and the connection reaches
only `/v1/phone-enroll/*` for at most 5 minutes. Once the box reports
`certified: true`, close it and enroll again with the same link (E3a). The
provisional attempt and the final one each use one of the link's 5
attempts. An older host ignores `enrollKey` and closes with 4040 as before.

### 2.10 Direct path and cleartext

The direct path is plain HTTP inside the tailnet or LAN, with a bearer token
([`ios-app.md`](ios-app.md), "Security model"). Android's network security
config can permit cleartext per domain, but not per IP range. So:

- `base-config cleartextTrafficPermitted="true"`.
- The app itself refuses any `http://` base URL whose host isn't one of
  these, mirroring iOS's `ATSPolicy`:
  - loopback;
  - RFC 1918;
  - link-local;
  - CGNAT `100.64.0.0/10`;
  - IPv6 ULA;
  - `*.ts.net`, `*.home.arpa`, `*.internal` or `*.lan`.
- The relay is always `https` with system trust (no user CAs, the default).
- No HTTP cache (OkHttp `cache(null)`), like `urlCache = nil` (IOS-6).
- Newer Android releases add a local-network-access permission. If the
  target SDK enforces it, request it when the user first adds a direct
  address, not at launch.

### 2.11 Secure clipboard

iOS copies secrets with `.localOnly` and an expiry. Android has no
cross-device flag or expiry, so:

- **Sensitive clips:** set `ClipDescription.extras[EXTRA_IS_SENSITIVE] = true`
  (API 33+; the literal key on 30–32) on every secret copy:
  - the phone code (600 s);
  - invite links (600 s);
  - the bearer admission code (120 s).

  This hides the content from the system's clipboard preview. Keyboards,
  OEM clipboard sync and companion apps may still read it, so the UI copy
  doesn't promise more than that.
- **Expiry:** remember what was copied and when. On each `ON_START` and on a
  timer while in the foreground, if the expiry has passed and the primary
  clip is still the app's (compare a label nonce), call `clearPrimaryClip()`.
  Android 10+ denies clipboard access in the background, so a clip copied
  just before leaving the app can outlive its expiry until the app returns.
  The UI says so for the admission code.
- **Never shared:** the bearer admission code has no share action. Only
  sealed `ligusadmit1:` codes can be shared (an iOS review fix).
- **Never copied:** the desktop's `ligusenroll1:` offer (Add to my
  devices) is never copied, shared or shown as text; it lives in memory
  only (section 3.9).
- **Public values:** the safety code and the forge device-flow user code are
  copied as plain clips, as on iOS. Markdown code blocks are plain too.

### 2.12 Screenshot and overlay protection

- `FLAG_SECURE` on the windows that show:
  - the phone code and fingerprint;
  - the Add to my devices waiting screen (it shows the fingerprint);
  - the review and phone-action sheet;
  - the passkey presence sheet;
  - the bearer admission code;
  - invite codes;
  - forge token entry.

  Compose dialogs and bottom sheets have their own windows, so apply the
  flag to each (`DialogProperties(securePolicy = SecureFlagPolicy.SecureOn)`).
  This is stricter than iOS, which only covers the app switcher. On Android
  a screenshot or screen recording of a code is the realistic leak, and
  the review sheet is where tapjacking would matter.
- **Tapjacking:** on the review sheet and the pair confirm sheet, set
  `setHideOverlayWindows(true)` (API 31+), and enable
  `filterTouchesWhenObscured` on the Sign / Approve / Pair buttons (all API
  levels).
- The privacy cover for the lock is in section 2.5.

---

## 3. Security requirements

Each requirement maps to an iOS behaviour or review finding and MUST have a
test (section 4).

### 3.1 Preimage checks and the review sheet

Flow, per [`serve-api.md`](../docs-site/docs/operate/reference/serve-api.md#phone-actions):

1. `prepare {deviceId, action}`;
2. if `presenceRequired`, the passkey (section 2.4), then `presence` → `{preimage}`;
3. check;
4. review;
5. Sign;
6. `commit {prepareId, signature}`.

- **Before prepare**, the app runs the core's `PhoneAction::malformed`:
  - a post is non-blank and at most 16,000 scalars;
  - an answer is non-blank and within `flow::MAX_ANSWER_CHARS`;
  - a digest is 64 lowercase hex characters;
  - a base commit is 40 or 64 hex characters;
  - `raises` has 1–16 unique entries;
  - memory content passes `memory::content_ok`;
  - a rationale is at most 500 characters.
- **Before signing**, the core's `check_phone_action_preimage` MUST pass. It
  checks, in order:
  - the `hive-envelope-v3\0` layout with no trailing bytes;
  - this workspace;
  - that scope, timestamp, stamp and payload each round-trip to the same
    JSON value (no unknown field);
  - the stamp: this phone's `deviceId`, `sigAlg: p256`, a human actor, no
    `runtimeEffect`, this account;
  - `lamport == logHead.l + 1` (1 with no head), and the head isn't
    `u64::MAX`;
  - that the event's scope is its own;
  - presence: field-equal to the phone's own assertion, with the challenge
    equal to the presence digest, or absent when none was run;
  - the action's log (the chat, or the config log for `clearLockdown` and
    `proposeMemory`);
  - a payload that is exactly the action.

  Use the refusal strings as the core returns them. Admissions run
  `admission::check_phone_admission_preimage` instead, with the iOS checks
  around it:
  - the box fingerprint recomputed from the pasted `ligusjoin1:` request
    (`"ligus-box-join-v1\0"`);
  - the expiry capped at the chosen TTL + 5 minutes.
- **The app's own additions** (each was an iOS fix):
  - a post whose message id is already in the chat is refused;
  - a vote or `requestApply` on a **file diff** needs a digest the phone
    recomputed from the content it shows. A missing or different digest
    signs nothing. The box's digest binds only for workflow or command JSON
    the phone can't reproduce;
  - the signed-lamport ledger (section 3.2);
  - one action in flight per chat;
  - a sheet is bound to the profile it opened on and cancelled when that
    host's state is cleared, so host A's preimage is never signed for host B.
- **The sheet shows only values parsed from the bytes** ("What this phone
  read"):
  - the action, the person it signs as, the chat;
  - the text or answer, the option and the question;
  - the vote, the proposal and the first 16 characters of the content digest;
  - the host (label · id prefix, always showing the id prefix) and what
    happens on commit;
  - the request;
  - the lockdowns it lifts (matched against the `/v1/guards` signals the bar
    was drawn from);
  - the memory title, note, tags, rationale and "Readable by";
  - "Passkey: confirmed for exactly this event".

  Any box-supplied string shown next to them is cleaned
  (`clean_display_name`) and truncated, so a right-to-left override or a
  newline can't forge a row. The box's `preview` only decorates the passkey
  step.
- **Auto-sign:** as on iOS, a plain `post` needing no passkey MAY sign
  without a sheet once the check passes. A post that went stale at commit
  is redrafted once, and the redraft **waits for a tap**, so the box can't
  collect two signed posts. Everything else always shows the sheet. A
  `committed` answer is the box's claim until the event stream shows it.

### 3.2 Signed-lamport ledger, failing closed

- Store `{"<workspace>/<log>": u64}`, the highest lamport signed per log, in
  the sealed store (section 2.3.4).
- Before signing: refuse a preimage whose lamport is below the stored one
  ("a lamport below one this phone already signed on that log").
- Record the new maximum **under a lock, before** the Keystore signs. A
  signature that then fails leaves a higher mark, which is safe.
- **Fail closed:** a ledger file that exists but won't decrypt or parse
  throws "this phone can't read the record of what it signed". Nothing is
  signed and the file is left untouched (iOS fix: it used to read as empty
  and get overwritten). A missing file is empty only on first use. Keep a
  marker in DataStore that the ledger was created, so a deleted file is
  caught too.

### 3.3 Host profiles and the pinned `/v1/me`

- Each host profile is its own sealed record: the direct config, the tunnel
  config, the path preference, `phone` (the current binding) and
  `phoneKnown` (the pinned one).
- `GET /v1/me` goes **through the tunnel only**. On the direct path the app
  doesn't call it (the box answers `403 secure_channel_required`).
  `403 device_not_certified` gives "not certified": show the code and the
  fingerprint.
- **Validate** the answer:
  - `device.id` is this phone's derived id;
  - `device.tier` is `"phone"`;
  - `device.fingerprint` equals the local fingerprint. Otherwise: "don't
    sign anything";
  - `configLogId` is `workspace_config_session_id(workspaceId)`;
  - `rpId` is a lowercase hostname;
  - `origins` are `https://<rp or subdomain>[:port]` or
    `android:apk-key-hash:`. The app accepts the core's policy shapes
    (section 2.4); iOS accepts only `https` here;
  - `account.role` ∈ {owner, admin, contributor, viewer}.
- **Pin the first answer** per device, even across a later
  `device_not_certified`. A later answer with a different workspace,
  account, member, `rpId` or `origins` is refused as "changed" until the
  person forgets the host. Role and names may change. Call `/v1/me` again
  after a `403 device_not_certified` from any phone-action route.

### 3.4 Revocation: relay versus box

- **Box revocation** (a GOAWAY with code 1 `revoked` inside the Noise
  session, which only the host can produce) wipes the whole profile: the
  tunnel keys and credential, the direct token and the push key. When no
  relay host is left, delete the FCM token. Show "This host revoked this
  phone".
- **Relay revocation** (close code 4003, or an `error` frame) is the relay's
  word. It removes only the tunnel credential and push key for that host. A
  direct pairing, if any, stays, and `relayOnly` becomes `auto`. Show a
  "remote access ended" notice (iOS fix: a 4003 used to wipe the direct
  pairing and push for every host).
- A revocation that arrives before its handler is installed is queued and
  delivered (an iOS fix).
- A 403 during pairing is terminal. 4041 is final: a new device key is
  needed.
- **Device revocation on the workspace** (`DeviceRevoked`) makes the box cut
  the pairing and rotate `serve.token` ([`relay-tunnel.md`](relay-tunnel.md)
  §13.2). The phone sees it as a box revocation. Other phones re-fetch the
  token with `GET /v1/tunnel/direct` after a direct 401.

### 3.5 No signing from notifications

- **Approve** is an `Activity` PendingIntent to `ligus://proposal/<id>`. It
  opens the proposal and the review sheet, which needs a tap to sign. It
  never runs a vote from a receiver.
- **Reply** uses `RemoteInput`, delivered to an `Activity`, not a
  `BroadcastReceiver`. The text goes through the normal `post` path with
  auto-sign off, so the sheet shows. A DM, or a reply the phone can't sign,
  goes into the composer instead. If the box isn't reachable within 20 s,
  the reply is queued, and queued replies later go through the sheet one at
  a time.
- Both actions set `setAuthenticationRequired(true)` (API 31+), so the
  device must be unlocked.
- **App lock:** when the app is locked, or will lock because its grace
  period has run out, Approve only opens the proposal behind the lock.
  Reply is neither sent nor queued, and a local notification says "Reply
  not sent: Ligus is locked" (IOS-2).
- Generic (undecryptable) pushes carry no actions.

### 3.6 Deep-link and notification id validation

A chat or proposal id from a link or a notification MUST be:

- exactly one path segment;
- non-empty and at most 128 characters after percent-decoding;
- not `.` or `..`;
- free of `/ ? # \` and control characters.

Otherwise the link is ignored with fixed text. Never build a URL by string
concatenation from an unchecked id. Encode it as a path segment (OkHttp
`HttpUrl.Builder.addPathSegment`).

### 3.7 Labels and names

- **The phone label** (in the `ligusphone1:` code) is clamped exactly as
  Rust's `clamp_label` does (`crates/ligus-runtime/src/phone_enroll.rs`),
  by calling it through the core:
  1. drop controls and U+00AD, U+061C, U+180E, U+200B–U+200F, U+202A–U+202E,
     U+2060–U+206F, U+FEFF and U+FFF9–U+FFFB;
  2. trim;
  3. cut at 64 UTF-8 bytes on a character boundary;
  4. trim the end again.

  The desktop refuses a code whose label isn't its own clamp. iOS uses a
  stricter clamp that is always a fixed point of Rust's. Android uses
  Rust's own, so the question doesn't arise.
- **Display names** from the box (people, agents, hosts, notification
  `title` and `sender`, sheet rows) go through `clean_display_name`
  (`crates/ligus-proto/src/names.rs`) before display. The vectors are
  `crates/ligus-proto/tests/vectors/display-names.json`.
- **Rendering:** Compose `Text` with `TextDirectionHeuristics` set to
  first-strong per value, so one value can't reorder its neighbours.
  Markdown links open only `http`, `https` and `mailto` (IOS-3).
- **Avatars** are `data:` URLs only, at most 200,000 characters, decoded
  with `ImageDecoder` and a target size of at most 256 px, so a
  decompression bomb can't exhaust memory (IOS-5).

### 3.8 Other carried-over findings

| iOS finding | Android requirement |
|---|---|
| IOS-1 relay-forged notifications | section 2.6.3: generic text on any failure, no actions, no link |
| IOS-2 notification actions ignored the lock | section 3.5 |
| IOS-3 Markdown link schemes | section 3.7 |
| IOS-4 pairing unlocked the app | section 2.5 |
| IOS-5 avatar decompression bombs | section 3.7 |
| IOS-6 on-disk URL cache | section 2.10 (no HTTP cache) |
| IOS-7 the link banner echoed the URL | section 2.7 |
| An unrestorable device key was silently replaced | section 2.3.1 |
| The bearer admission code went to a synced pasteboard and a share sheet | section 2.11 |
| An admission's expiry could be stretched by the box | section 3.1 (TTL + 5 min cap) |
| The 4040 retry wasn't capped | 5 attempts per link |
| The phone code text was selectable | not selectable; Copy is the only path |
| Invite calls on the plain direct path | invite and forge-token calls go through the tunnel whenever the host has remote access |

### 3.9 Add to my devices (reverse QR)

The flow and the wire formats are in [`ios-app.md`](ios-app.md), "Add a
phone by scanning the desktop's QR", and in
[`design/v2/phone-enroll-reverse-qr.md`](design/v2/phone-enroll-reverse-qr.md).
Android MUST:

- hold **one offer at a time**: a second scan while a request is pending
  asks "Cancel the request to *Ana*'s desktop and use this code instead?";
- refuse an offer with `exp` more than 120 s in the past or more than 600 s
  ahead (`expired`, `too_far_ahead`);
- show the confirm sheet from `POST /v1/phone-enroll/check`, never from the
  QR, with the warnings: `nameCollision` ("Two people here are called Ana.
  Make sure this is your desktop.") and another account (this phone's
  pinned `/v1/me` `account.id` on another host profile differs from the
  offer's `accountId`);
- treat **success as the box's `certified: true`** for this phone, not the
  desktop's answer;
- check every answer with `check_answer` before showing it, and on
  `AnotherPhone(certified)` show the tripwire alarm ("Another phone was added
  with this code. Tell the person at the desktop to revoke it.") and not
  retry;
- keep the offer's secret, `k_req`, `k_ans` and the offer text in memory
  only, zeroized after the flow, never logged, never in a crash report,
  `toString()` or error string;
- show "Update ligus serve, or use Can't scan?" on a 404 from the routes or
  a 4040 without a provisional answer, and "This box can't reach the
  workspace's relay. Use Can't scan?" on `503 relay_unavailable`.

---

## 4. Testing

### 4.1 Golden vectors (all upstream files)

The app's test suite reads the vectors from this repo, pinned to a commit
and copied into `rustcore/src/test/resources/golden/` by a sync script that
checks their SHA-256 against a manifest:

| File | What the Android tests check |
|---|---|
| `crates/ligus-core/tests/golden/device_id_v1.jsonl` | every P-256 line derives its `deviceId`. Ed25519 lines too, through the core, to prove the algorithm string matters |
| `crates/ligus-core/tests/golden/phone_actions_v1.jsonl` (25 lines) | each `ok` case passes `check_phone_action` and its `signature` verifies under `publicKey`. Each `refuse/*` case fails with exactly its `expect` string. The `*/presence` cases use their `assertion` and `challenge` |
| `crates/ligus-core/tests/golden/p256_cryptokit.jsonl` | the signatures verify over `preimage`, and `der_to_raw_low_s` plus Keystore-style DER round-trips them |
| `crates/ligus-core/tests/golden/envelopes_v3.jsonl` (117 lines) | the JSON round-trip used by the parser accepts every envelope's fields |
| `crates/ligus-core/tests/golden/presence/*.json` | `check_assertion_locally` over real authenticator output |
| `crates/ligus-proto/tests/vectors/display-names.json` | `clean_display_name` |
| `crates/ligus-core/tests/golden/phone_enroll_qr_v1.jsonl` | the offer, the keys (`rid`, `k_req`, `k_ans`), the request and the answer, and each negative with its refusal reason (`EnrollError::code`) |
| `crates/ligus-core/tests/golden/differential/rust_parity_enroll_v1.jsonl` | mutated offers, requests and answers with the core's verdict. Through the shared core these are the core's own parsers, so the boundary parity test covers them |
| `docs/relay-tunnel/vectors.json` | V-keys, V-cred, V-noise (as initiator, in both modes, with injected ephemerals), V-mux, V-push, and every `negative` case (§19.2) |

Two fixtures exist only in the iOS repository: the 600-line label-clamp
corpus and the 240-case Rust parity corpus. **Upstream them** to
`crates/ligus-core/tests/golden/` (as `phone_label_clamp_v1.jsonl` and
`rust_parity_v1.jsonl`, generated by a Rust test), so both apps read one
copy.

### 4.2 Rust differential harness

With the shared core the parser *is* the core. What still needs a
differential test is the boundary, plus the parts Kotlin does itself:

- **Boundary parity.** A JVM test loads the host build of `ligus-mobile` and
  runs the 240-case parity corpus, plus a seeded mutation corpus (bit flips,
  truncations, length-field edits, JSON edits from the golden preimages,
  10k cases per run), through the binding. It compares each verdict and
  refusal string with a Rust test that runs the same corpus directly. Any
  difference fails the build.
- **DER → raw low-S.** Property tests: random keys and messages signed by
  the Keystore (on an instrumented device) or by BouncyCastle (on the JVM)
  must convert and verify, with `s ≤ n/2` every time. Malformed DER
  (non-minimal, trailing bytes, negative, more than 33 bytes) is refused.
- **Tunnel against the reference.** A JVM test runs the Android tunnel stack
  (Kotlin mux streams over a fake WebSocket) against the Rust host connector
  in process through a test-only UniFFI export, and checks T-api-parity
  semantics: `GET /v1/health`, SSE with `Last-Event-ID`, flow control,
  RESET and GOAWAY.
- **Fuzzing.** `cargo fuzz` targets in `ligus-mobile` for the mux decoder,
  the pair-link parser, the `hpc1` parser and `push_open`, run in CI for at
  least 10 minutes (§19.2 V-mux).

If the owner chooses a Kotlin reimplementation instead (section 2.9), the
iOS-style differential fuzz becomes **mandatory** before M2. That means
91k+ mutated golden preimages and raw JSON compared with the Rust core,
covering UUID spellings, f64 overflow, nesting depth 127, duplicate keys and
byte-exact string comparison.

### 4.3 Instrumented and UI tests

These run on an emulator in CI (`reactivecircus/android-emulator-runner`,
API 30 and the current API):

- **Keystore:** key generation, hardware-level reporting (the emulator is
  software, allowed only in the debug test build), signing, an
  unrestorable-key error path, and backup exclusion.
- **Sealed store:** atomic writes, a corrupt-file error, and the ledger
  failing closed.
- **Compose UI tests:**
  - pairing (scan replaced by a link injection);
  - refusals 4040/4041/4042/4001;
  - the review sheet showing parsed values with hostile box strings;
  - FLAG_SECURE set on every listed window (assert the
    `WindowManager.LayoutParams` flags);
  - lock behaviour (cold start, grace, pairing doesn't unlock);
  - notification actions while locked;
  - deep-link id rejection.
- **Push:** a test `FirebaseMessagingService` input (the
  `RemoteMessage` builder) with the `push` vector `ct`, a forged `ct`, a
  replayed `seq` and a stale `ts`. Only the first shows content, and the
  others show the generic text with no actions.
- **Demo mode** end to end with no network.

### 4.4 No real biometrics or passkeys in CI

- **BiometricPrompt** sits behind an `Authenticator` interface. CI uses a
  fake that succeeds, fails or is cancelled. A manual checklist covers real
  hardware.
- **Credential Manager** sits behind a `PasskeyClient` interface. CI uses a
  fake backed by the core's software authenticator (`presence::soft`,
  exported only in a test feature of `ligus-mobile`). It produces real
  ES256 assertions with origin `android:apk-key-hash:<test cert hash>`
  against a test workspace whose policy lists that origin. A real-passkey
  run is a manual release check on two devices (Pixel with StrongBox, and
  one non-Pixel).
- **FCM** isn't contacted in CI. The relay's FCM sender is tested against a
  fake FCM endpoint in the relay repo.

### 4.5 Interop (nightly or opt-in)

As the iOS `TunnelInteropTests` do, start the real Go relay
(`LANGSTROTH_RELAY_TUNNEL=open`) and a real `ligus serve`, with an isolated
`LIGUS_DATA_DIR`, on the CI runner. Then pair the emulator over the relay,
run a `post` phone action end to end, and check that it appears in the
event stream. This job runs nightly, not per PR.

---

## 5. Distribution

### 5.1 Channels

| Channel | Pros | Cons | Recommendation |
|---|---|---|---|
| **Google Play** | the reach users expect; FCM; automatic updates | Play App Signing; review; the data safety form; a developer account | **Yes**, starting with internal testing, then a closed track, then production |
| **F-Droid** | the self-hosting audience; builds from source | needs a public repository; no Firebase (non-free); F-Droid signs builds itself unless reproducible builds with the developer's signature are set up, which gives a *different* signing certificate and so a different Android origin and `assetlinks` entry | **Later**, as a `foss` flavor with UnifiedPush, after the repository question (section 5.4) |
| **Direct APK** (GitHub Releases; Obtainium for updates) | no gatekeeper; fast; the same `foss` flavor | users must allow installs; no automatic updates without a tool | **Yes**, alongside Play, signed with the same app signing key |

### 5.2 Signing keys and Play App Signing

- Generate the **app signing key** yourself (RSA 4096 or EC P-256), offline,
  and back it up in two places. Enroll it in **Play App Signing with "use my
  own key"**, then use a separate **upload key** for Play uploads.
- With one app signing key, the Play and direct-APK builds share one
  certificate. That gives them:
  - one `android:apk-key-hash:` origin in the workspace presence policy;
  - one `sha256_cert_fingerprints` entry in `assetlinks.json`;
  - updates between channels without uninstalling.

  If Google generates the key instead, the direct APK needs a second key,
  which means a second origin, a second `assetlinks` entry, and no
  cross-channel updates.
- Debug and `.lab` builds use their own key. Their origin goes only into
  test workspaces' policies.
- Key rotation (APK Signature Scheme v3 lineage) changes the origin. Plan a
  policy update across workspaces before rotating.

### 5.3 Data safety form notes

These notes need checking against Play's policy at submission time.

- **Data the developer collects: none.** The app sends data only to the
  user's own box and the user's chosen relay. The developer runs no backend
  that receives user data. This matches the iOS label "Data Not Collected".
- **FCM:** Firebase Cloud Messaging processes a device registration token.
  The payload is end-to-end encrypted, and no analytics or Crashlytics are
  included. Declare whatever Play's current guidance requires for FCM
  tokens. Turn off Firebase's automatic data collection
  (`firebase_messaging_auto_init_enabled` false until the user opts in to
  push, and no Analytics dependency).
- Encrypted in transit: yes (Noise over TLS to the relay). The direct path
  is plain HTTP inside the user's own network or tailnet. Explain that in
  the form's notes.
- Deletion: forgetting a host deletes its data on the phone, and
  uninstalling deletes everything.
- **Permissions:** `INTERNET`; `CAMERA` (QR only); `POST_NOTIFICATIONS`;
  `USE_BIOMETRIC`; and, if the target SDK enforces it, the local-network
  permission. No location, contacts, storage or `QUERY_ALL_PACKAGES`.
- A privacy policy URL is required. Use the same page as iOS
  (`ligus.ai/privacy`), extended for Android.
- **Reviewer access:** the app is unusable without a box, so the review notes
  point to **Try the demo**.
- **Export compliance:** the app implements its own end-to-end encryption
  (Noise, ChaCha20-Poly1305). Play has no export questionnaire, but US
  export rules still apply to the publisher. The iOS app's `app-review.md`
  notes the same question, and its shipped plist answer
  (`ITSAppUsesNonExemptEncryption: false`) contradicts its own docs. Settle
  it once for both apps.

### 5.4 Owner decisions

1. **Package name.** It is permanent on Play and appears in `assetlinks.json`,
   the relay's `LANGSTROTH_RELAY_FCM_APPS` and the Android origin. Options:
   - `ai.ligus.android` (*recommended*). It matches the desktop's
     `ai.ligus.desktop` and the domain that serves `assetlinks.json`.
   - `com.zazulam.ligus`. It matches the iOS bundle id
     (`com.zazulam.ligus`, dev `com.zazulam.ligus.lab`), which lives on a
     personal developer account.

   Either way, the dev flavor appends `.lab`.
2. **Repository visibility.** The iOS repository is private. Options:
   - A public Android repository: the protocol, vectors and box are already
     public, it is required for F-Droid, and it invites review of exactly
     the code that signs. *Recommended.*
   - A private one, mirroring iOS. That rules out F-Droid and makes outside
     audit harder.
3. **Play developer account:** a personal account or an organization
   account under the Langstroth organization. This affects the account name
   shown on Play, and verification requirements.
4. **Firebase project ownership.** It holds the FCM sender. A self-hoster
   rebuilds with their own.

---

## 6. Milestones

Size estimates are for one experienced Android engineer, with a Rust
engineer for the core work. They are rough.

| Milestone | Scope | App size | Server / relay / desktop / site changes |
|---|---|---|---|
| **M0 Core** | the `crates/ligus-mobile` UniFFI crate (section 2.9 surface); moving the tunnel helpers and `phone_enroll`'s code functions into a shared crate; cargo-ndk build; the vector sync script; upstreaming the iOS-only corpora | 2–3 weeks (Rust) | This repo only: the new crate (including `phone_enroll_qr`'s phone side), CI targets for Android ABIs, fuzz targets. No wire change |
| **M1 Pairing + chat read** | Keystore device key and id; This phone (code, QR, fingerprint, FLAG_SECURE, sensitive copy); pairing v1 and v2 (direct and tunnel: Noise, mux, HTTP, SSE, renewal, direct-token refresh); refusals 4040/4041/4042; **Add to my devices** (the CameraX + ML Kit scanner for `ligusenroll1:`, E3b provisional enrollment, section 3.9); `/v1/me` pinning; host profiles and the path policy; chats, messages (read, live stream), people, channels, chat details, agents with their availability (`available`, `unavailableReason`, `access`), dashboard, appearance; app lock; deep links; demo mode; ntfy links work | 6–8 weeks | None (E3b and the `/v1/phone-enroll/*` routes ship in `ligus serve` 0.1.1). ntfy is the notification fallback |
| **M2 Signed actions + review sheet** | phone actions `post`, `vote`, `answer`, `clearLockdown`, `requestApply`, `withdrawApply` with the full check and sheet; the lamport ledger; passkeys through Credential Manager; the Review tab (proposals, diff, rendered Markdown, votes, discussion, request changes, Apply on ‹host›, proposal sets, guards, Flow); typing | 5–7 weeks | **Site:** `assetlinks.json` on ligus.ai. **Desktop:** an Owner affordance to add the app's `android:apk-key-hash:` origin to the workspace presence policy (`WorkspaceRulesSet.presence`), and new workspaces include it at genesis. Optionally, the default policy gains the published app's origin |
| **M3 Push** | FCM registration, decryption and notification channels; Reply and Approve actions (no signing from notifications); push settings (`PUT /v1/tunnel/push`); foreground suppression | 2–3 weeks app | **Relay:** the FCM push provider (section 2.6.3), about 1–2 weeks with tests. Relay operators set the FCM env. No `ligus serve` change |
| **M4 Memory, repos, admissions, identity** | `proposeMemory` and memory and task-state views; the repo picker, clone, pull, PR/MR, code hosts; Admit a box (`ligusjoin1:` and admission sheet); safety codes, invites, DMs | 4–5 weeks | None |
| **M5 (optional) FOSS flavor** | UnifiedPush; ZXing scanner; F-Droid metadata; reproducible build | 3–4 weeks | **Relay:** a UnifiedPush/Web Push provider with SSRF guards |

Every milestone also gets the adversarial security review pass and
code-verified docs before it lands, as each iOS phase did.

---

## 7. Design and experience notes

The sections above fix what the Android app must do. This section says how
it should feel, so that the two phone apps, the desktop and `lt` read as
one product. It carries the iPhone app's design direction across to
Material 3, and it notes where the iPhone app itself is changing, so that
Android starts from the newer design rather than copying the older one.
The security rules in section 3 always win over anything here.

### 7.1 Calm by default

Every screen has one primary focus; everything else is one tap away. When
two designs are otherwise equal, choose the one that shows less at once.

- **One question first.** The phone's main job is to answer "does anything
  need me, and is my agent still working?". The home destination is an
  **Inbox** (section 7.2), not the chat list.
- **One badge.** Only the Inbox destination and the launcher show a count,
  and the count is only items where this person is the next actor: votes
  they can still cast, agents' questions to them, workflow gates assigned to
  them, signing and passkey requests, device and box requests, invites, and
  failures of runs they started. Unread messages are a dot on the chat row,
  never a badge. The same count appears on every surface.
- **At most one banner,** and only for states that block work (a chat in
  lockdown, the workspace's restricted mode). The demo notice shows once,
  not on every destination. Confirmations are a single snackbar. Errors stay
  where they happened (inline, with a retry) and in a "Recent problems"
  list under the person's profile; they never disappear on a timer.
- **Restrained colour.** Neutral surfaces; the accent marks an action or a
  state. Status colours appear only with a glyph (section 7.9).
- **Quiet motion.** Standard transitions about 140 ms, sheets about 220 ms,
  one short pulse on a state change, no animation per streamed token, and
  nothing that flashes. When the system's animations are off, show static
  text ("working · 3:12") instead of spinners.
- **Plain words.** Operational labels use plain language: "Workspace" for the
  queue and task board, "Workspace notes" for marks, "Restricted mode" for
  dearth mode. Wire ids and stored keys don't change. Use one noun for the
  machine that runs `ligus serve`: **box** (with "computer" only where a
  person is told to type a command on it).

### 7.2 Navigation and information architecture

Four destinations, in a `NavigationSuiteScaffold` (a navigation bar on
phones, a rail on tablets and unfolded foldables):

| Destination | What it holds |
|---|---|
| **Inbox** | *Needs you* (oldest deadline first), *Running* (this person's agents and runs, with Follow), *Done today* (collapsed). The only badge. |
| **Chats** | Direct messages, then chats by channel; a state chip on each row (working, queued, needs you, failed). |
| **Review** | All proposals, with filters: *Needs my vote*, *Waiting on others*, *Mine*, *Workspace* (the queue and task board). |
| **Agents** | Agents with their live state and "runs on" host, the hosts list, and the dashboard as a *Health* section. |

- **Settings** opens from the person's avatar in the top app bar on every
  destination: This phone and devices, notifications, appearance,
  security, privacy, code hosts, connection, people and safety codes,
  about. The workspace and host switcher sits at the start of the same bar.
- **List and detail:** `ListDetailPaneScaffold` on wide windows, so the list
  stays beside the open chat or proposal (the iPad split view's
  counterpart).
- **Deep links and notifications** open the item's own screen with the
  Inbox beneath it in the back stack, so Back returns to the Inbox.
- **Edge-to-edge** is the default from target SDK 35: draw behind the system
  bars, pad content with `WindowInsets` (`safeDrawing`), and keep the
  composer above the keyboard with `imePadding()`. Lists scroll under a
  translucent navigation bar; the composer and the vote bar never do.
- **Predictive back** (`android:enableOnBackInvokedCallback="true"`):
  - Use the system's back preview for every destination and sheet;
    `PredictiveBackHandler` only where a screen must intercept back.
  - Back on the signing sheet means **Cancel**. It never signs, and while a
    signature is being committed back is ignored (as the sheet's dismissal
    is blocked on iOS).
  - Back inside Add to my devices returns to the previous step. A request
    that is already pending stays pending (the flow lives in the model);
    leaving shows where it is when the person comes back.
  - Back from a chat with an unsent draft keeps the draft.

### 7.3 Material 3 equivalents

| iPhone app | Android (Compose, Material 3) | Note |
|---|---|---|
| Tab bar that shrinks on scroll | `NavigationSuiteScaffold`; the bar hides on scroll with an exit-always scroll behaviour on long lists | Four destinations (section 7.2) |
| Glass buttons for controls | Filled, tonal and outlined buttons; no blur | Content cards stay solid so text contrast is predictable |
| Sheets with medium and large detents | `ModalBottomSheet` (partially expanded, then expanded); full-screen dialogs for multi-step flows | `SecureFlagPolicy.SecureOn` where section 2.12 requires it |
| Navigation bar with large title | `LargeTopAppBar` collapsing to `TopAppBar` | Titles in sentence case |
| Searchable list | `SearchBar` in the top bar of Chats and Review | |
| Pull to refresh | `PullToRefreshBox` | |
| Swipe actions on rows | `SwipeToDismissBox` only for non-committal actions: Snooze and Mark done in the Inbox, Archive in Chats | **Never** approve, reject or answer by swipe |
| Context menu | Long press opens a `DropdownMenu` (Copy, Mention, Propose as a memory note) | Also reachable as TalkBack custom actions |
| Alert | `AlertDialog` | Only for destructive confirmations |
| Transient banner | `Snackbar`, one at a time, for confirmations only | Errors persist (section 7.1) |
| Empty state view | A centred icon, one line and one action | No illustrations beyond the mark |
| Haptic feedback on approve, reject, failure | `HapticFeedbackConstants.CONFIRM`, `REJECT` | Nothing on swipe |
| Status capsules | `AssistChip` or a small label with a leading Material Symbol | Glyph always present (section 7.9) |
| Lock screen view and privacy cover | An opaque composable above all windows; `setRecentsScreenshotEnabled(false)` | Section 2.5 |

### 7.4 Notifications

Notify only when this person is the next actor, or when something they
started finishes or fails. Everything else waits in the app. This refines
the channel list in section 2.6.4.

| Channel | Contents | Importance | Default | Actions | Grouping |
|---|---|---|---|---|---|
| **Needs you** | votes this person can cast, agents' questions to them, workflow gates assigned to them | high | on | **Review** (opens the proposal or gate), **Open** (questions) | per chat or proposal |
| **Your runs** | runs and turns this person started: done | low (silent) | on | Open | one summary per burst ("@coder and @docs finished") |
| **Your runs: failed** | the same, failed | default | on | Open | per chat |
| **Mentions and direct messages** | `@you` mentions and DMs | default | on | Reply (`RemoteInput`, section 3.5) | per chat, `MessagingStyle` |
| **Devices and pairing** | new phone, device and box requests; "new phone paired" | high | on | Open | none |
| **Other** | pushes that couldn't be decrypted (generic text) | low | on | none | one summary |

- **Every other chat message is not notified.** The host is told which kinds
  this phone wants (an additive field on `PUT /v1/tunnel/push`).
- **The proposal action is labelled "Review", not "Approve".** It behaves
  exactly as section 3.5 describes: it opens the proposal, and nothing is
  signed from the notification.
- **Lock screen privacy:** `VISIBILITY_PRIVATE` with a public version that
  names only the event ("An agent asked a question"). The title names the
  sender and the event, never message text; the preview text is in the body
  only when previews are on. The in-app previews setting defaults to
  "When unlocked".
- **Bursts:** updates within about 60 s collapse into the group summary;
  `setOnlyAlertOnce(true)` on updates to an existing notification.
- **Badges:** `setShowBadge(true)` only on the Needs you channel, so the
  launcher dot means "something needs you".
- **Settings:** Settings → Notifications lists the channels in plain words
  with their current state and links to the system channel screen
  (`ACTION_CHANNEL_NOTIFICATION_SETTINGS`); it doesn't duplicate the
  system switches.
- **Do Not Disturb:** no channel bypasses it by default; the person can
  allow Needs you through in system settings.
- **Permission timing:** ask for `POST_NOTIFICATIONS` once, after the first
  pairing and the lock offer, with one sentence on what will notify.

### 7.5 Following a running agent

When the person sends a message that starts a turn from the phone, starts a
workflow, or taps **Follow** on a Running row, show one ongoing notification
for it (one per chat; a single "3 agents working" notification when more
than two are followed).

```
┌──────────────────────────────────────┐
│ Ligus · working · 3:12               │
│ @coder · Flaky checkout test         │
│ Running the suite (step 3 of 5)      │
│ ━━━━━━━━━━━━━━━━━━━━━━━░░░░░░░░░░    │
│ [ Open ]                             │
└──────────────────────────────────────┘
```

- **Android 16 and later:** use `Notification.ProgressStyle` (steps and
  segments for workflow stages) and request promotion to a Live Update
  (`POST_PROMOTED_NOTIFICATIONS`), so the state shows as a status-bar chip
  and on the lock screen. Check `canPostPromotedNotifications()` and fall
  back quietly.
- **Older versions:** an ongoing notification with a determinate or
  indeterminate progress bar and a chronometer (`setUsesChronometer`).
- **States:** working (with an optional step), queued (host busy), needs
  you, done, failed, each with its glyph (section 7.9). Updates are silent;
  the notification alerts once, when the state becomes *needs you* or
  *failed*. On done or failed it becomes a normal notification with a
  one-line summary and is dismissed after 15 minutes (`setTimeoutAfter`).
- **Where updates come from:** while the app is in the foreground, the
  event stream; in the background, the encrypted push the app already
  decrypts (section 2.6.4). Because Android decrypts pushes in the app, the
  progress text can stay end-to-end encrypted; keep the payload minimal
  anyway (state, step, timestamps, ids) and take names from local data.
  Don't run a foreground service just to watch an agent.
- **No push yet:** without push, show the notification while the app is
  open and mark it "last update 2 min ago" when it goes stale; never show a
  state the app can't vouch for.
- **Stop** is not a notification action. Opening the notification shows
  the chat, where stopping is a deliberate action.
- **Lock screen:** the same `VISIBILITY_PRIVATE` rule: "An agent is working"
  without previews.

### 7.6 Widgets, Quick Settings and shortcuts

Home-screen widgets are out of scope for v1 (section 1.3). When they come:

- **Glance widget,** small: the Needs-you count only. Medium: the
  Needs-you list, with titles hidden while the device is locked. A tap opens
  the item. **No widget approves, answers, signs or stops anything.**
- **Quick Settings tile:** not recommended; it would add a control without a
  clear job. Revisit only if people ask for a one-tap Inbox.
- **App shortcuts** (long press on the launcher icon): Inbox and New chat,
  static; recent chats as dynamic shortcuts, without message text.

### 7.7 Approving a proposal or a vote on a small screen

The pattern is "what you see is what you sign": the confirmation restates
the action in human terms, bound to what is signed, and nothing approves
from a list or a notification.

- **Read first.** The proposal screen shows who decides (who approved, at
  which role, how many more are needed), what will run, the changes with a
  file list, then the discussion. The vote bar is not pinned at first: it
  appears once every file has been expanded or scrolled past (a per-file
  "seen" mark), or after an explicit "I've read it" on long diffs.
- **The signing sheet restates the change:** the title, the file count, the
  `+`/`−` line counts, the file names, the version, and what approving does
  ("Counts 1 of 2. Runs nothing; Implement runs on the desktop."). The
  digest is a disclosure, not the headline. If the proposal was revised
  after it was opened, the sheet says so and returns to the diff. Reject
  uses the same sheet, with an optional reason, so both outcomes are equally
  deliberate.
- **Presence:** when the workspace needs a passkey for the vote, the button
  says so ("Approve with passkey"), so the Credential Manager sheet that
  follows is expected.
- **Tapjacking and capture** protections from section 2.12 apply to this
  sheet.

### 7.8 Biometric prompt, scanner and Add to my devices

**Biometric prompt (app lock, section 2.5):**

- Title "Unlock Ligus"; no subtitle or description. With
  `DEVICE_CREDENTIAL` allowed there is no negative button, so the system
  offers the PIN or pattern itself.
- One automatic prompt per return to the foreground. After a cancel, the
  lock screen shows a single **Unlock** button; never loop the prompt.
- Turning the lock on from Settings takes one successful prompt with the
  title "Turn on app lock".

**Scanner** (CameraX with ML Kit; ZXing in the FOSS flavor):

- Full screen, with one line saying what to point at ("Point at the code on
  your computer's screen"), a framing guide, a torch toggle and a close
  button.
- Purpose-limited: the pairing scanner takes `ligus://pair` links, the
  enrollment scanner takes only `ligusenroll1:` codes, and anything else
  gets "That's not a Ligus code" without leaving the scanner.
- A confirm haptic and a TalkBack announcement ("Code scanned") on success.
- A camera-denied state with a button to the app's system settings and the
  alternative ("Paste the link instead" or "Can't scan? Show this phone's
  code"), also reachable from inside the scanner.

**Add to my devices** (section 3.9):

- A step indicator: Name, Scan, Confirm, Check.
- The fingerprint in groups of four in a large monospace style that wraps
  rather than shrinks, with an optional word form from the same word list
  as the desktop, and read to TalkBack character by character.
- A countdown ring plus text for the code's expiry.
- Instructions as a numbered list using the desktop's real labels.
- `FLAG_SECURE` on the waiting step (section 2.12).

### 7.9 Tokens, colour and status

The palettes come from the shared token file (`ligus-brand-tokens.json`),
authored in OKLCH with light and dark values, the same source the desktop
and the iPhone app use. Map the shared roles onto the Material 3 colour
scheme, and keep the rest as extended colours in a `CompositionLocal`:

| Shared role | Material 3 slot | Note |
|---|---|---|
| `canvas` | `background`, `surface` | |
| `panel` | `surfaceContainer` | cards, sheets |
| `mist` | `surfaceContainerHigh` | |
| `line` | `outlineVariant` | |
| `ink`, `ink-soft` | `onSurface`, `onSurfaceVariant` | |
| `accent-cool` (honey) | `primaryContainer` (a fill) | never text on its own |
| accent ink | `primary` | links, icons, focused controls |
| `accent-warm` | `tertiary` | "a person is involved": human turns, needs you. Never a status. |
| `danger` | `error` | |
| `ok`, `info`/`running`, `warn`, `neutral` | extended colours | each with an ink and an 8% tint |

- **Warning has its own hue, away from honey:** light `oklch(0.62 0.15 50)`,
  dark `oklch(0.78 0.14 55)`, always with a `!` glyph.
- **Glyph and colour together, always.** Status is never colour alone, so it
  survives colour-blindness and the system's colour correction:

| State | Role | Material Symbol |
|---|---|---|
| Working / running | info | `progress_activity` (static text when animations are off) |
| Needs you | accent-warm | `diamond` (filled) |
| Queued / host offline | warn | `hourglass_empty` |
| Done / applied | ok | `check_circle` |
| Failed / error | danger | `cancel` |
| Paused | neutral | `pause_circle` |
| Warning / degraded | warn | `warning` |
| Unknown / no data | neutral | `help` (outlined) |

- **Diffs** carry `+`/`−` gutter glyphs as well as the add and delete tints.
- **Dynamic colour** stays an opt-in palette, off for "Match host"
  (section 1.2).
- **Dark theme** is the shared dark palette, not an automatic inversion.
  Support the system contrast setting (Android 14 and later,
  `UiModeManager.getContrast()`) with medium- and high-contrast variants
  generated from the same tokens.
- **Shape and spacing:** a 4 dp grid; corner radius 8 dp for controls,
  12 dp for cards, the Material default for sheets.
- **Bee references** stay in the app icon and the brand mark on the connect
  screen; none in operational UI.

### 7.10 Accessibility

- **TalkBack:**
  - Merge each list row into one node with a full sentence
    (`semantics(mergeDescendants = true)`), as the iPhone app does.
  - Expose row actions as `CustomAccessibilityAction`s (Snooze, Mark done,
    Copy, Mention).
  - Mark section titles as headings (`semantics { heading() }`) so people
    can jump between Votes, What will run, Changes and Discussion.
  - Announce state changes once with a polite live region ("coder
    finished", "coder asks a question", "Vote recorded"). Don't re-read a
    streaming reply on every token; announce it when it completes.
  - Read the fingerprint and codes character by character.
  - Label icon-only buttons with the same words the screen shows elsewhere.
- **Font scale:** Android 14 and later scale text non-linearly up to 200%.
  Use `sp` for all text, avoid fixed heights, and at large scales:
  - collapse the notes above the chat composer into one line;
  - move the reply target into the composer's menu;
  - let an agent's question scroll with the transcript;
  - stack row metadata vertically.

  Screenshot tests run at 100%, 150% and 200%; the chat transcript must stay
  visible at 200%.
- **Touch targets:** at least 48 × 48 dp, including the inline answer options
  and chips.
- **Motion:** honour "Remove animations" (the animator duration scale is 0).
  No automatic scrolling while the person reads earlier turns; offer
  "Jump to latest" instead.
- **Colour:** glyph plus colour everywhere (section 7.9); verify every
  status pair under the system's colour-correction modes and at 3:1
  non-text contrast.
- **Keyboard and switch access:** visible focus, a logical focus order, and
  Enter to send in the composer with a hardware keyboard.

---

## 8. Open questions for the owner

1. **Package name:** `ai.ligus.android` (recommended) or `com.zazulam.ligus`?
2. **Public or private repository** for the Android app (public recommended;
   F-Droid requires it)?
3. **Shared Rust core:** adopt UniFFI for Android (recommended)? Should the
   iOS app move onto the same crate later, retiring its Swift port and parity
   corpus?
4. **Device key auth:** keep parity with iOS (Keystore key not
   auth-bound; the passkey gates high-value actions), or offer an opt-in
   auth-bound key at creation (per-signature biometric, re-certified after
   lock-screen changes)?
5. **Presence policy for Android:** the owner signs with one app signing key
   for Play and the APK (recommended), and adds its
   `android:apk-key-hash:` origin to `DEFAULT_ORIGINS` for new workspaces. Is
   the Owner affordance for existing workspaces acceptable in M2?
6. **Push:** approve the relay's FCM provider (section 2.6.3) and a Firebase
   project for the published build? Is UnifiedPush wanted, and when?
7. **Key attestation:** should the desktop show (or require) Android key
   attestation, proving StrongBox or TEE, when it certifies an Android
   phone? That would be a certification-flow change on the desktop.
8. **Pairing link hardening:** add a verified https pairing link
   (`https://ligus.ai/pair#…`, secrets in the fragment, App Link verified)
   so another app can't intercept `ligus://pair` on Android? It would need a
   `ligus pair` change and `handle_all_urls` in `assetlinks.json`.
9. **Screenshot policy:** Android uses `FLAG_SECURE` on code and sheet
   windows, which is stricter than iOS. Should iOS add the equivalent (hide
   content during capture), or should Android match iOS?
10. **Export compliance:** settle the encryption answer once for both apps
    (section 5.3).
11. **Play developer account:** personal or organization?

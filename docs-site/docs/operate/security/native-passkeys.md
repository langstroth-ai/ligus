# Native passkeys on the desktop

When something needs your [passkey](index.md#presence), the desktop app asks
the operating system's own passkey dialog where it can, and opens your
browser where it can't. What gets signed is the same either way: the same
event digest, checked the same way by every device.

| OS | Native ceremony | When the browser is used instead |
|---|---|---|
| macOS 13+ | AuthenticationServices: Touch ID, iCloud Keychain, an iPhone nearby | the workspace's relying party isn't one the app is associated with, the build isn't signed with the entitlement, or the dialog fails |
| Windows 10 1903+ | `webauthn.dll`: Windows Hello, security keys, a phone | `webauthn.dll` is missing, or the dialog fails |
| Linux | none (there is no platform API) | always |

The confirmation sheet in the app shows what you are authorizing and the
digest's first characters in every case. While the system dialog is up, the
sheet has **Use the browser instead** (for a security key or another device
the dialog doesn't offer). If you close the system dialog, the sheet stays
and offers the browser, your recovery key (to confirm an action) or Cancel.

Set `LIGUS_NATIVE_PASSKEYS=0` to always use the browser.

One confirmation runs at a time: a second one (another action that needs
your passkey, or adding a passkey while one is pending) waits until the
first ends. Recovery key words answer only the request whose digest the
sheet showed when you entered them; words that arrive after that request
ended are dropped, never applied to the next one.

## What stays the same {#same}

- **The challenge.** The passkey signs the event digest
  (`ligus-presence-v1`), on every path. The desktop checks the
  `clientDataJSON` names its own challenge before using a result, and
  every device verifies the assertion in the fold with the same verifier.
- **The options.** User verification required, attestation `none`, ES256
  or EdDSA, the account's registered credentials allowed (sign-in) or
  excluded (a new passkey).
- **The origin.** On macOS the system writes the `clientDataJSON` with origin
  `https://<relying party>`, so the native path is used only when the
  workspace's presence policy accepts that origin. On Windows the app writes
  it with the origin of the workspace's `/presence` page, the same origin the
  browser hand-off produces.

## While a confirmation is pending {#pending}

A confirmation runs inside the write that needs it, and the app's service
lock is held until it ends (up to three minutes). About fifty of the app's
window commands (listing chats, members, proposals and the like) run on
the app's main thread and take that lock, so if the window refreshes during
a confirmation the main thread waits for it to end. Meanwhile the window
doesn't respond, the sheet's buttons included; the passkey dialog or the
browser page still completes the confirmation.

The macOS dialog needs the main thread to appear and to answer. The app
gives it 3 seconds to appear and checks every second that the main thread
still answers; after 5 seconds without an answer it cancels the dialog and
opens the browser, which doesn't need the main thread. So on macOS a busy
window sends you to the browser rather than freezing the dialog. Whichever
path answers, the result of the other is dropped, and a ceremony's result is
used only after its `clientDataJSON` names the requested digest; one
approval confirms one event. On Windows the dialog runs on the
confirmation's own thread with the main window as its owner; how it behaves
while the main thread waits hasn't been tested on a real device.

Releasing the lock during the ceremony would mean splitting every write
that can need a passkey into a prepare step and a commit step, as
`ligus serve` does for its two phone-confirmed actions. The desktop's writes
return results computed after the append, and some append several events,
so this is a per-command change, not done here. Running the window's
commands off the main thread (Tauri `async` commands) would also keep the
window responsive.

## macOS: the association {#macos}

macOS serves a passkey to an app only for a relying party (RP) that both
sides claim:

1. **The app** is signed with the entitlement
   `com.apple.developer.associated-domains = ["webcredentials:ligus.ai"]`
   (`app/Ligus.entitlements`, application identifier
   `H5H3NNR456.ai.ligus.desktop`). The app reads the same file to decide
   which RPs it tries natively.
2. **The domain** serves `https://ligus.ai/.well-known/apple-app-site-association`
   (JSON, no redirect, `Content-Type: application/json`) listing the app:

```json
{"webcredentials":{"apps":["H5H3NNR456.ai.ligus.desktop"]}}
```

The site's file also lists the iPhone apps. Apple's CDN fetches and caches
it, so a change can take a day to reach Macs.

**Self-hosted relying parties.** A workspace whose RP is your own domain
keeps using the browser hand-off on macOS: the published app is associated
only with `ligus.ai`. To go native, build the app with your domain added to
`app/Ligus.entitlements`, sign it with your own team, and serve the
association file above on your domain with your team and bundle id. Windows
needs none of this.

### Signing the entitlement {#signing}

Associated domains is a restricted entitlement: a Developer ID app that
claims it must embed a **Developer ID provisioning profile** for
`ai.ligus.desktop` with the Associated Domains capability, or macOS refuses
to launch it. So the entitlement is signed in only by the passkeys build
overlay, `app/tauri.passkeys.conf.json`, which also embeds the profile:

```bash
# app/embedded.provisionprofile: the Developer ID profile (never committed)
cargo tauri build --config app/tauri.passkeys.conf.json
```

The release workflow does this when the `APPLE_PROVISIONING_PROFILE` secret
(the profile, base64) is set, next to the other `APPLE_*` signing secrets.
Without it, releases build as before: no entitlement, and macOS passkeys use
the browser. Local builds signed with an Apple Development identity (no
profile) also use the browser.

Check a signed build:

```bash
codesign -d --entitlements - /Applications/Ligus.app   # lists webcredentials:ligus.ai
security cms -D -i /Applications/Ligus.app/Contents/embedded.provisionprofile | grep -A3 associated-domains
```

## Testing on real devices {#manual-test}

The automated tests never open a system passkey dialog: they drive the
native path with a software authenticator behind the same interface, and
check the result format against the W3C WebAuthn test vectors (Apple's
among them). These need a person and real hardware:

**macOS** (a build from the passkeys overlay, installed in /Applications):

1. Settings → Passkeys & keys → **Add a passkey**: the macOS passkey sheet
   appears over the Ligus window (not the browser). Use Touch ID; then the
   second prompt (confirming the registration) appears the same way.
2. Remove a member or rotate the workspace key: the sheet shows
   "Confirm with your passkey in the system dialog", Touch ID confirms it,
   and the event is accepted on another device.
3. Press **Use the browser instead** while the dialog is up: the dialog
   closes and the browser opens `ligus.ai/presence`.
4. Close the system dialog: the sheet offers the browser, the recovery key
   and Cancel; the recovery key words confirm the action.
5. Add a second passkey from the same Mac: on macOS 14.4+ the dialog refuses
   (the account already has one here); on older macOS the browser opens.
6. A workspace with a self-hosted RP: the browser opens directly.
7. An unsigned or Apple Development build: the browser opens (the log notes
   the association error).

**Windows** (Windows 10 1903 or later): the same steps 1 to 4 with Windows
Hello, plus a USB security key through the same dialog, and a workspace
with a self-hosted RP (the native dialog is used).

**Linux**: unchanged, the browser hand-off.

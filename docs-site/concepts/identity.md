# Identity & devices

Ligus distinguishes three layers of identity:

| Layer    | Stable? | Where it lives                                            |
|----------|---------|-----------------------------------------------------------|
| Account  | yes     | public records as JSON in the app data dir; private seed in `keys/` |
| Device   | yes per device | public in the device roster; private Ed25519 seed in `keys/` |
| Workspace member | per workspace | the SQLite event store, as `memberAdded` events |

The app data directory is `ai.ligus.desktop` (e.g. on macOS
`~/Library/Application Support/ai.ligus.desktop/`).

## Account

An `HumanAccount` represents you-the-person across machines. It
carries a display name, a handle (`@you`), and an Ed25519 *account*
public key. The account key is the root of trust for every device
you authorize.

You get one account per Ligus install on first launch. The
generation is silent — Ligus reads `$USER`/hostname for defaults you
can edit in onboarding.

## Device

A `DeviceIdentity` represents one machine. It has:

- A device-specific Ed25519 keypair (private seed on disk in the
  data dir's `keys/`, public in the workspace device roster).
- A `DeviceCertificate` signed by the account key, asserting "this
  device belongs to this account."

Every signed envelope carries the `signerDeviceID`. Receiving peers
look up the device's public key in the workspace device roster and
verify.

## Workspace membership

When you join a workspace, an `memberAdded` event lands in the log
with your `ActorIdentity` (id + display name + kind). Subsequent
events from your devices verify against the rosters.

## What travels in invites

A workspace invite code (`hivews1:…`) is base64url-encoded JSON of the
workspace connection: its display name, the relay URL, the room, **the
workspace key (the E2EE passphrase)**, and, for the founder check, the
founding identity (and a DM's account, for direct messages). See the
[invite code reference](../reference/invite-payload.md).

So an invite code **is** a secret: anyone holding it can sync and read the
workspace. Share it only over a channel you trust. Inviting by GitHub handle
avoids pasting it: the key is sealed to the invitee's devices instead. See
[Invites & joining](../features/invites.md).

It doesn't contain private signing keys, API credentials or workspace
content. The joiner's devices publish their own keys and certificates
through their own events.

## Key storage

Ligus stores private key seeds as raw files in the `keys/` subdirectory of
its data directory (`ai.ligus.desktop` for the desktop, `LIGUS_DATA_DIR` for
the CLI), written with the process's default file mode (on many Linux
systems that is readable by other local users; keep the data dir `0700`).
Protect them with your OS account and disk encryption; anyone who can
read these files can act as your device. An OS-keystore-backed vault
(macOS Keychain / Windows Credential Manager / Secret Service) is a
planned drop-in behind the same interface but is **not** what ships
today, so don't assume OS-keystore protection.

## Direct peer-to-peer links

Desktops that are contacts of each other can also sync directly, device to
device, over [iroh](https://www.iroh.computer/) (QUIC), falling back to the
relay when no direct connection can be made (**Settings → Team sync →
Direct peers**). Events on a direct link are the same signed events as on
the relay, verified the same way; set `LIGUS_DISABLE_P2P` to turn direct
sync off. See [Security model](../security/index.md#encryption).

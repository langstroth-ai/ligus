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

Ligus keeps private key seeds, and every other secret it holds, in the OS
secret store:

| Where | Backend |
|---|---|
| macOS (desktop and CLI) | a dedicated keychain file, `secrets/ligus.keychain-db` in the data dir, unlocked by a random password kept in your login keychain (an item only the Ligus binary can read without a prompt: by signature for a release app, by code hash for a development build or the CLI, which ask once after each rebuild). If the keychain can't be used, Ligus refuses rather than fall back to files; `LIGUS_SECRET_BACKEND=file` opts in |
| Linux desktop | the Secret Service of your session (GNOME Keyring, KWallet, KeePassXC), through `secret-tool` |
| Headless Linux (`ligus serve`, `ligus worker`) | `systemd-creds` (TPM2 when present, else the host key), credential files under `credentials/` in the data dir |
| Kubernetes | Secret files mounted at `LIGUS_SECRETS_DIR`; anything not provisioned there falls back to files |
| Anywhere else | owner-only files (`keys/*.seed` and `secrets/`, `0600` in `0700` dirs), with a warning in Settings |

`LIGUS_SECRET_BACKEND` (`keychain`, `secret-service`, `systemd-creds`, `k8s`,
`file`) picks one. `settings.json`, `config.toml` and the ligus-auth session
file hold `{"set": true}` where a secret was; a plaintext secret found in one
is removed, not imported (2.0 starts from scratch; enter it again).

Agents can't reach the store: the data dir (keychain file included) is
denied to every agent process by the OS sandbox, even to subscription CLIs
that keep login-keychain access, and the session bus never reaches an agent.

!!! warning "Backups on macOS"
    A backup of the data dir includes the Ligus keychain file but not its
    unlock password, which stays in your login keychain. Restoring on another
    Mac needs that login keychain too (or start that Mac as a new device).

## Direct peer-to-peer links

Desktops that are contacts of each other can also sync directly, device to
device, over [iroh](https://www.iroh.computer/) (QUIC), falling back to the
relay when no direct connection can be made (**Settings → Team sync →
Direct peers**). Events on a direct link are the same signed events as on
the relay, verified the same way; set `LIGUS_DISABLE_P2P` to turn direct
sync off. See [Security model](../security/index.md#encryption).

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

An `HumanAccount` represents you-the-person across machines. It is an
Ed25519 *account* key and an **account id**: a random UUID made with the key
on first launch. The account key is the root of trust for every device you
authorize.

No provider signs you in. The account id is never derived from anything
public (not a GitHub or other forge id, not an email, not a handle), and it
never changes after first launch. More devices join your account through an
enrollment code (`hivedev1:`, Settings → Profile → **My devices**), never by
signing in somewhere.

## Profile

Your profile is a **display name** and an optional **picture** (a small
`data:` image cropped on your device). Both ride on your identity to every
member of your workspaces. A name is not unique and is never used to look
anyone up: two people can both call themselves "Ada". Ligus has no global
handle namespace, because no Ligus server could own one.

Your **commit email** (`git_email`) is a profile setting too: it credits you
as the author of commits an agent makes on your behalf. Members of your
workspaces can see it.

## Safety code

Your **safety code** is the fingerprint of your account key: the first 16
bytes of SHA-256 of the key, shown as eight groups of four hex characters
(for example `3f2a 91c0 77de 0b14 c2e8 5a6f 9d31 e047`). It is the same value
an invite carries as `fk`, so:

- Settings → **Profile** shows yours.
- **People** shows each co-member's, next to their name.
- An invite shows its founder's before you join.

Compare codes out of band (in person, on a call) the first time you connect
with someone. When two people share a name, the safety code tells them apart;
when an invite claims to come from someone you know but carries another
code, Ligus says so and won't join it.

## Device

A `DeviceIdentity` represents one machine. It has:

- A device-specific Ed25519 keypair (private seed in the OS secret store,
  public in the workspace device roster).
- A `DeviceCertificate` signed by the account key, asserting "this
  device belongs to this account."

Every signed envelope carries the `signerDeviceID`. Receiving peers
look up the device's public key in the workspace device roster and
verify.

## Workspace membership

When you join a workspace, an `memberAdded` event lands in the log
with your `ActorIdentity` (id + display name + kind). Subsequent
events from your devices verify against the rosters.

## Who you can reach

You reach people in two ways, and neither involves a directory:

- **An invite link or QR code** you hand over yourself.
- **Co-members**: anyone you share a workspace with. Their devices'
  key-agreement keys are signed on that workspace's log, so Ligus can seal
  an invite to them, or open a direct message, without asking any server
  who they are. See [People, contacts & DMs](../features/collaborators.md).

## What travels in invites

A workspace invite code (`hivews1:…`) is base64url-encoded JSON of the
workspace connection: its display name, the relay URL, the room, **the
workspace key (the E2EE passphrase)**, the founder's account id and safety
code (`fk`), a single-use invite id and, for a direct message, the account
it is addressed to. See the [invite code reference](../reference/invite-payload.md).

So an invite code **is** a secret: anyone holding it can sync and read the
workspace. Share it only over a channel you trust. An invite to a co-member
avoids pasting it: it is sealed to their verified devices and dropped into
their contact box on the relay. See [Invites & joining](../features/invites.md).

It doesn't contain private signing keys, API credentials or workspace
content. The joiner's devices publish their own keys and certificates
through their own events.

## Forge tokens stay on the device that uses them

A token for GitHub, GitLab, Forgejo, Gitea or Codeberg is for repository
work, not identity. It lives only in the OS secret store of the device or box
that does the git work (scope `forge`, a separate secret for read and for
write), and it is never synced, never put in an invite, a contact record or a
log, and never sent to a relay: the relay client has no way to carry one.

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

Desktops that share a workspace can also sync directly, device to
device, over [iroh](https://www.iroh.computer/) (QUIC), falling back to the
relay when no direct connection can be made (**Settings → Team sync →
Direct peers**). Events on a direct link are the same signed events as on
the relay, verified the same way; set `LIGUS_DISABLE_P2P` to turn direct
sync off. See [Security model](../security/index.md#encryption).

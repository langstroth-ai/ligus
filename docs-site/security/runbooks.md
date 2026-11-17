# Security runbooks

Step-by-step responses for the incidents you're most likely to have. Each
says what the step achieves and what it doesn't. Background:
[Security model](index.md), [Threat model](threat-model.md).

!!! info "Two different levers"
    **Revoking** a device or removing a member stops their **new events**
    from being accepted. **Rotating** the workspace key stops them from
    **reading new traffic**. You usually need both. **Remove & revoke** on a
    member (People pane) does both; **Settings → Passkeys & keys → Rotate
    workspace key** rotates on its own. A rotation is a signed event on the
    workspace's config log, sealed to the key-agreement keys every remaining
    member's devices published themselves, `ligus serve` and `ligus worker`
    boxes included. Both need a [passkey confirmation](index.md#presence).

## Revoke one of your devices {#revoke-device}

For a `ligus serve` box (or another device) enrolled as **your** device.

1. On a desktop: **Settings → Account → My devices → Revoke** on that
   device. You can't revoke the device you're using.
2. Effect: updated devices quarantine its events from then on; a revoked
   `ligus serve` goes read-only and reports `identity.mode` `revoked` on
   `/v1/health`.
3. Not achieved: it still holds the workspace key and can read whatever it
   can still fetch. If the device itself is lost or compromised, continue
   with [Compromised device](#compromised-device).
4. **Revoking needs Admin.** Device revocation is an Admin-level change on
   the workspace log, and every device ignores one signed by anyone below
   Admin, both when it checks the event and when it builds its list of
   trusted devices. If you aren't an Admin, your revocation has no effect on
   other devices: ask an Admin to revoke the device, or to remove that
   device's member (for a box that was its own member), and treat it as a
   [compromised device](#compromised-device) meanwhile.
5. To re-enroll the same box later:
   `ligus serve enroll --new-device`, then approve the new code.

## Lost phone {#lost-phone}

The phone holds no workspace key, but it can act through your box.

On the box, with the server's environment loaded:

```sh
set -a; . ~/.config/ligus-serve/ligus-serve.env; set +a
ligus phones list
ligus phones revoke "Alice's iPhone"       # or its pid
ligus phones list                          # it now shows `revoked`
```

`ligus phones revoke`:

- ends the phone's live sessions at once and refuses its requests;
- tells the relay, which then refuses that phone before it reaches the box
  (retried every 30 s until the relay acknowledges);
- **rotates `serve.token`**, because the phone may also hold the direct-path
  token. Every phone that uses the direct path must pair again
  (`ligus pair`). Pass `--keep-token` only if the lost phone never used the
  direct path. There is no per-phone revocation on the direct path: revoking
  any direct-path phone means rotating `serve.token` for all of them.

A phone paired **only** on the direct path has no entry in
`ligus phones list`. Rotate the token by hand; a running `ligus serve`
picks up the new file within 2 seconds:

```sh
umask 077
openssl rand -hex 32 > "$LIGUS_DATA_DIR/serve.token"
```

Then re-pair your remaining phones. If the phone app's Face ID lock was on,
the thief also needed your face or passcode; revoke anyway.

!!! warning "Pairing codes in the journal"
    `ligus serve --pair` prints the pairing link to stdout, and with a
    direct path that link carries `serve.token`. Under the shipped systemd
    unit stdout goes to journald, where it is kept and readable by root and
    by the groups your distribution lets read journals (such as
    `systemd-journal` or `adm`). Don't put `--pair` in a unit; pair with
    `ligus pair` from a terminal. If a box ever started with it, rotate
    `serve.token` (above) and re-pair.

## Compromised device {#compromised-device}

A laptop, server or worker whose data dir may have been copied. Its holder
can act as that device and read everything it could.

1. **Cut its writes.**
    - Your own device enrolled to your account: [revoke it](#revoke-device).
    - A box that is its own member (a worker, or a serve box not enrolled as
      yours): an Admin removes that member in the **People** pane.
    - A teammate's device: they revoke it in their **My devices**; if they
      can't, an Admin removes the member.
2. **Cut its relay access.** On a gated relay, revoke its token: remove it
   from `LIGUS_RELAY_ACCESS_TOKENS` and restart, or (signed tokens) see
   [Relay token compromised](#relay-token).
3. **Cut its reads.** It holds the workspace key. After step 1, **rotate the
   workspace key** ([below](#rotate-workspace-key)): the new key is sealed
   only to devices that are still certified and whose account is still a
   member, so the revoked device can't read anything sent after it. Its old
   key can't be used to write either: every device refuses events sealed
   under an older key by someone who is no longer a member, and a rotation
   it published itself is never adopted (rotations are signed by an Admin's
   device and checked on every device).
4. **Rotate what it held besides Ligus keys**: model API keys, MCP tokens,
   `LIGUS_WS_SECRET_*` values, the `gh` login of a serve box, Git
   credentials.
5. A compromised `ligus serve` also held phone credentials:
   `ligus remote reset` on its replacement gives new host keys; pair phones
   again.
6. If your account has a passkey, the stolen device can't
   add a device for you or rotate your account key without that passkey, and
   if your credentials were cleared it can't register a passkey of its own
   without an Admin's re-grant. Don't approve a passkey confirmation you
   didn't start, and remove a passkey that lived only on the stolen device
   (**Settings → Passkeys & keys**).

## Offboarding a person {#offboarding}

1. **Admin, People pane → Members → Remove & revoke** on the person. This
   removes them from the roster and rotates the workspace key, sealed to
   every remaining member's devices (desktops, `ligus serve` and `ligus
   worker` boxes). The toast says whether the key was rotated ("No relay
   configured, so the key wasn't rotated" means it wasn't). Your browser
   asks for a passkey for each of the two events.
2. If the relay enforces membership, also remove them in **Settings → Team
   sync → Team members** (or the relay roster). That alone doesn't rotate
   the key; step 1 does.
3. Revoke their relay token ([below](#relay-token)).
4. If they had access to shared worker or serve boxes, remove their logins
   there and rotate any secrets they could read.
5. Headless boxes follow the rotation on their next sync, as long as they
   published their key-agreement key (they do on start). **Settings →
   Passkeys & keys** shows the key version; on a box, `ligus sync` logs it.
6. They can't enroll themselves back: a removed account's self-enrollment is
   refused on every device until an Admin adds them again.
7. Remember: they keep everything they had already synced, and every
   earlier key epoch (old epochs stay readable by design).

## Move to a new workspace {#move-to-a-new-workspace}

The complete way to cut someone's reading, including on headless devices.

1. **Add workspace → Create** on an Admin's desktop. It gets a new random
   room and key. **Copy invite**.
2. Re-create the workspace configuration there (agents, runtimes, skills,
   channels). `ligus config export` / `ligus config import` moves workspace
   runtimes (inline secrets are exported as `secretRef`s).
3. Invite the people who stay (from People if you share another workspace
   with them, or the invite link over a trusted channel).
4. Re-enroll headless boxes with **new** data dirs:
   `ligus enroll 'hivews1:…'`, then restart them.
5. Archive the old workspace. Its history stays on the devices that had it.

## Rotate the workspace key {#rotate-workspace-key}

1. An Admin opens **Settings → Passkeys & keys → Rotate workspace key**
   (confirm with a passkey). The new key is sealed to every
   current member's devices that published a key-agreement key, including
   the serve and worker boxes enrolled for a current member (a box enrolled
   for someone who was removed gets no new key, even while an agent is still
   bound to it); the rotation is a signed `WorkspaceKeyRotated` event on
   the config log, one version above the last one.
2. Every device adopts it from the config log; its body also goes to the
   relay keyring route so a device whose log is behind can read it sooner.
   A device seals new events only under the newest **authorized** rotation,
   never under a key it merely found on the relay.
3. Check: **Settings → Passkeys & keys** shows the new key version on each
   desktop; a box's log says `encrypted, key version N`.
4. If two Admins rotate at the same moment, the one that sorts first wins
   everywhere; events already sealed under the other key stay readable, and
   its issuer simply rotates again.
5. A device that isn't a recipient yet (it joined after the last rotation
   from an old invite) reads what it can and writes nothing until the next
   rotation includes it. Invites carry every authorized epoch key, so a
   fresh invite avoids that.

## A new device on a workspace {#confirm-device}

When your account has a passkey, a new device of yours (a second laptop
signed in with the same account) doesn't count on a workspace until
that passkey confirms it: other devices don't accept its changes, and it
gets no new workspace keys. Open **Settings → Passkeys & keys** on it and
choose **Confirm this device**. Ligus never asks for this on its own in the
background. A serve box you enroll is confirmed when you approve its
enrollment on your desktop (also with your passkey).

## Lost a passkey {#lost-passkey}

1. If you have another passkey or your recovery key: **Settings → Passkeys
   & keys**, register a replacement (confirm with the one you still have),
   then remove the lost one (an Admin can also remove it for you, unless
   you are an Owner: an Admin never removes the passkeys of a role above
   theirs).
2. Synced passkeys (iCloud Keychain, Google Password Manager) usually come
   back with the account; a lost YubiKey doesn't.
3. Lost every passkey but the recovery key: when the **Confirm with
   passkey** sheet opens, choose **Use recovery key** and enter the 24 words
   (they are checked and used on this device only, never stored), then
   register new passkeys.
4. Lost everything: another Owner (or, for an Admin or below, an Admin)
   removes your passkeys (with their own passkey), then **re-grants** you in
   the **People** pane (**Allow new passkey**, confirmed with their passkey),
   and you register a new first passkey. Your device alone can't register
   one without that grant (so a stolen device can't either), and a grant is
   good for one registration. **A sole Owner without a recovery key can't
   make admin changes, by design.** That is why creating a workspace asks
   for the recovery key right after the passkey.
5. A workspace founded from the CLI has no passkey yet: open it in the
   desktop app and create one and a recovery key (Settings → Passkeys &
   keys). The founder's first passkey needs no one else's grant; until it
   exists nobody can join. Someone an Admin adds can register their first
   passkey right away.

## The Ligus keychain on macOS {#mac-keychain}

- Ligus's secrets live in `secrets/ligus.keychain-db` in the data dir; its
  unlock password is a login-keychain item named `ai.ligus.secrets.unlock`
  whose access list trusts only the Ligus binary.
- **A development build** (or the CLI) is trusted by its code hash: after
  each rebuild or upgrade macOS asks once for your login keychain password
  to let it read the unlock item. Choose **Always Allow**. A release app is
  trusted by its signature (bundle id and team), so updates don't ask.
- **If the keychain can't be used**, Ligus won't save secrets and says why in
  **Settings → Passkeys & keys**. Fix the keychain (unlock it, or remove a
  stale `ai.ligus.secrets.unlock` item together with the keychain file to
  start over), or start Ligus with `LIGUS_SECRET_BACKEND=file` to keep its
  secrets in owner-only files instead.

## Recovery key {#recovery-key}

- Created in **Settings → Passkeys & keys → Create recovery key**: 24 words
  (BIP-39), shown once, never stored by Ligus. Write them down; keep them
  offline.
- It is an Ed25519 key registered like a passkey and accepted wherever a
  passkey is. Treat it like a master password: anyone with the words and one
  of your devices can make admin changes as you.
- Creating a new one doesn't remove the old one; remove it in the same
  screen once the new one is written down.

## Relay token compromised {#relay-token}

| Gate | Revoke |
|---|---|
| `LIGUS_RELAY_ACCESS_TOKENS` | remove the token from the list and restart the relay |
| Signed `hrt1` tokens | the open-source relay has no per-token revocation list: tokens stay valid until they expire. Issue them with `--exp-days`, and with `--workspace <id>` under `LIGUS_RELAY_WORKSPACE_SCOPE` so a leaked token reaches only its own workspaces. To cut one off now, generate a new issuer key (`ligus-relay keygen`), set the new public key, restart, and re-issue tokens to everyone else |
| Relay with an admin API (downstream builds) | revoke the token there; takes effect immediately |

A relay token only grants use of the relay. Without the workspace key it
reads nothing.

## Issuer key leaked {#issuer-key}

Anyone with the issuer seed can mint relay tokens. Generate a new pair, set
`LIGUS_RELAY_TOKEN_PUBKEY` to the new public key, restart, re-issue tokens.
Paste the key carefully: since relay v0.2.0 a value that isn't a valid
Ed25519 public key stops the relay from starting rather than leaving it
ungated.
Remove the old seed from **Settings → Team sync → Advanced → Relay issuer
key** on any desktop that held it (it lives in the OS secret store, not in
`settings.json`).

## `serve.token` leaked {#serve-token}

Write a new one (see [Lost phone](#lost-phone)); the server switches within 2
seconds; re-pair direct-path phones. Tunnel phones are unaffected.

## Box host keys leaked {#host-keys}

`ligus remote reset` makes new host keys and a new host id; every tunnel
phone must pair again. If the relay uses a tunnel allowlist, replace the old
host id with the new one in `LIGUS_RELAY_TUNNEL_HOSTS`.

## After any incident {#after}

- Check each device's guard log (the `guard_signals` table in `hive.db`) for
  ingest rejections around the time of the incident.
- Consider turning on [dearth mode](../features/guards.md#dearth-mode) while
  you clean up: new members and devices then need two Admins.

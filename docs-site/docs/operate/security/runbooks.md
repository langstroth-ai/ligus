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

1. On a desktop: **Settings → Profile → My devices → Revoke** on that
   device. You can't revoke the device you're using.
2. Effect: updated devices quarantine its events from then on; a revoked
   `ligus serve` goes read-only and reports `identity.mode` `revoked` on
   `/v1/health`.
3. Not achieved: it still holds the workspace key and can read whatever it
   can still fetch. If the device itself is lost or compromised, continue
   with [Compromised device](#compromised-device).
4. **Your own device needs no Admin.** Revoking a device your account
   holds is self-service at any role: every device accepts it, both when
   it checks the event and when it builds its list of trusted devices.
   Your account holds a device when it certified it and no other member's
   account certified the same key for themselves (a device id is derived
   from its public key, so anyone who knows the key could). Anything else
   still needs an Admin, and never one ranked below the device's holder: a
   device another member also certified, a device nobody certified here
   (a phone known only by its pairing), or a box that is its own member.
   If yours is one of those, ask an Admin to revoke the device, or to
   remove that device's member (for a box that was its own member), and
   treat it as a [compromised device](#compromised-device) meanwhile.
5. To bring a box back later: start it from a fresh data dir and admit it
   again ([Admitting a box](../ops/admitting-a-box.md)).
6. **A phone is revoked for good under its key.** Its device id comes from
   its Secure Enclave key, which survives reinstalling the app. My devices
   says which box a phone is paired with and when it was last seen, and the
   confirmation names the phone you're holding ("This is the iPhone paired
   with home-server right now"): check it before you revoke. To use a
   revoked phone again, on the phone choose **Reset this phone's key** (on
   the screen that says it was revoked, or **This phone was revoked? Reset
   its key** on the first screen of a phone with no workspace), confirm
   with Face ID or the passcode, then add it again with **Add to my
   devices**
   ([Revoking a phone, and getting it back](../../ligus/features/phone-app.md#revoked-phone)).

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

Also revoke the phone's device key on your desktop
([Revoke one of your devices](#revoke-device)), so no member accepts what it
signs. If you find the phone again, it comes back only with a new key
(**Reset this phone's key** in the app, then **Add to my devices**).

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
    - A box (a serve or worker box, always its own service member): an
      Admin removes that member in the **People** pane, then rotates the
      workspace key. If it still had an unredeemed admission Secret, delete
      it.
    - A teammate's device: they revoke it in their **My devices**; if they
      can't, an Admin removes the member.
2. **Cut its relay access.** On a gated relay, revoke its token: remove it
   from `LANGSTROTH_RELAY_ACCESS_TOKENS` and restart, or (signed tokens) see
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
2. If the relay enforces membership, also remove them from the relay
   roster (**People → Relay members**). That removes them from the
   workspace roster too, if step 1 hasn't, and rotates the key again.
3. Revoke their relay token ([below](#relay-token)).
4. If they had access to shared worker or serve boxes, remove their logins
   there and rotate any secrets they could read. A box is its own member:
   removing a person, even the Admin who admitted a box, leaves the box in
   the workspace. Remove the boxes they ran on their own
   ([Remove a box](#remove-box)).
5. Headless boxes follow the rotation on their next sync, as long as they
   published their key-agreement key (they do on start). **Settings →
   Passkeys & keys** shows the key version; on a box, `ligus sync` logs it.
6. They can't enroll themselves back: a removed account's self-enrollment is
   refused on every device until an Admin adds them again.
7. Remember: they keep everything they had already synced, and every
   earlier key epoch (old epochs stay readable by design).

## Remove a box {#remove-box}

For a `ligus serve` or `ligus worker` box that is its own member (see
[Admitting a box](../ops/admitting-a-box.md)).

1. **Admin, People pane → Members → Remove & revoke** on the box's member,
   or, on the desktop that generated its bootstrap, **People → Relay members → Remove** on its `ligus-box:` identity. Either one removes
   the box's member from the workspace roster and then rotates the
   workspace key to the members who remain. The relay-roster path also
   takes the box off the relay, and for a box that never joined it closes
   the box's admission.
2. Effect: the rotation leaves the box out, so it **loses access to new
   traffic** from then on; every device refuses what it signs afterwards.
3. Check the toast. If it says the entry matches no workspace member (a
   box bootstrapped elsewhere or before 0.1.1), the box may still read new
   traffic: do step 1 with **Remove & revoke** on its member. If removing
   the member failed, the key was not rotated: retry with **Remove &
   revoke**.
4. Not achieved: the box keeps everything it had already synced and every
   earlier key epoch. Rotate any secrets it held (`LIGUS_WS_SECRET_*`, its
   relay token, forge logins).
5. To bring it back, start it from a fresh data dir and admit it again.

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
   `ligus enroll -` (paste the new invite, then Ctrl-D), then restart them.
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
  whose access list trusts only the Ligus binary. The items inside the
  Ligus keychain (service `ai.ligus.secrets`) trust the same binary.
- **A development build** (or the CLI) is trusted by its code hash: after
  each rebuild or upgrade macOS asks once for your login keychain password
  to let it read the unlock item. Choose **Always Allow**. A release app is
  trusted by its signature (bundle id and team), so updates don't ask.
- Ligus reads the unlock password once per launch and keeps it while it
  runs, so a prompt you answer with **Allow** doesn't come back on every
  use. If the Ligus keychain locks again (sleep, `security lock-keychain`),
  Ligus unlocks it with the password it already holds.

### macOS keeps asking about `ai.ligus.secrets.unlock` {#keychain-prompt}

This happens when the keychain items were saved by a **different Ligus
build** than the one you're running (a test build, or a development build),
for example after you switch to the Homebrew release. macOS shows *Ligus
wants to use your confidential information stored in
"ai.ligus.secrets.unlock" in your keychain*.

1. Enter your **login** password and choose **Always Allow** (Allow also
   works, once).
2. A signed release of the Ligus app then repairs the items by itself: it
   checks the password against the Ligus keychain, re-creates the unlock
   item with the same password under an access list that trusts this
   release (and only it: other apps you allowed are dropped), writing the
   new item before it removes the old one, and does the same for the items
   inside the Ligus keychain. That needs no prompt: Ligus authorises it
   with the Ligus keychain's own password, which it holds. From then on
   nothing asks. The `ligus` CLI and development builds don't repair
   items; run the build that saved them.
3. **If macOS asks for the "ligus" keychain password**, choose **Cancel**.
   That password is random, only Ligus knows it, and you never need to
   type it; quit and reopen the signed release instead, and it repairs the
   item.

A development, ad-hoc or unsigned build never rewrites an item's access
list: it would pin the items to an unverified binary. Only a build that
satisfies Ligus's designated requirement (Apple-anchored, Ligus's bundle id
and team id) repairs them.

If you choose **Deny** or **Cancel**, the desktop says *macOS didn't let
Ligus read its keychain unlock password* and offers **Try again**; nothing
is changed and your keys stay in the keychain. Over SSH or from a
background service macOS can't ask at all, and the error says so: run the
same build once in the Mac's desktop session (the app, or the same `ligus`
command with the same data dir in a desktop terminal) and choose **Always
Allow**.

`ligus doctor` checks this without prompting (it opens the keychain, and
reads every item, with keychain prompts off) and reports
`secrets.keychain-access`:

| Result | Meaning |
|---|---|
| ok | this build reads the Ligus keychain without a prompt |
| warn: the unlock password exists, but this build isn't trusted | macOS asks; run this build in the desktop session, enter your login password, choose Always Allow |
| warn: N items were saved by a different Ligus build | reopen a signed release of the app; it repairs them without asking |
| fail: the unlock password is missing | the item really isn't in the login keychain (a data dir copied from another Mac or user) |
| skip | no Ligus keychain in this data dir yet |

### If the keychain can't be used

Ligus won't save secrets and says why in **Settings → Passkeys & keys**.
Unlock the login keychain, or allow Ligus as above. Don't delete
`secrets/ligus.keychain-db` or the `ai.ligus.secrets.unlock` item: this
device's keys are in that keychain, and removing either loses them (the
device would have to be enrolled again). To keep secrets in owner-only
files instead, start Ligus with `LIGUS_SECRET_BACKEND=file`.

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
| `LANGSTROTH_RELAY_ACCESS_TOKENS` | remove the token from the list and restart the relay |
| Signed `hrt1` tokens | the open-source relay has no per-token revocation list: tokens stay valid until they expire. Issue them with `--exp-days`, and with `--workspace <id>` under `LANGSTROTH_RELAY_WORKSPACE_SCOPE` so a leaked token reaches only its own workspaces. To cut one off now, generate a new issuer key (`langstroth-relay keygen`), set the new public key, restart, and re-issue tokens to everyone else |
| Relay with an admin API (downstream builds) | revoke the token there; takes effect immediately |

A relay token only grants use of the relay. Without the workspace key it
reads nothing.

## Issuer key leaked {#issuer-key}

Anyone with the issuer seed can mint relay tokens. Generate a new pair, set
`LANGSTROTH_RELAY_TOKEN_PUBKEY` to the new public key, restart, re-issue tokens.
Paste the key carefully: since relay v0.2.0 a value that isn't a valid
Ed25519 public key stops the relay from starting rather than leaving it
ungated.
Remove the old seed from **Settings → Team sync → Advanced → Relay issuer
key** on any desktop that held it (it lives in the OS secret store, not in
`settings.json`).

## `serve.token` leaked {#serve-token}

Write a new one (see [Lost phone](#lost-phone)); the server switches within 2
seconds; re-pair direct-path phones. Tunnel phones are unaffected.

## `serve.operator.token` leaked {#operator-token}

No phone holds it, so nothing needs re-pairing. Replace it on the box:

```sh
openssl rand -hex 32 > "$LIGUS_DATA_DIR/serve.operator.token"
chmod 600 "$LIGUS_DATA_DIR/serve.operator.token"
```

The server switches within 2 seconds. Whoever held it could use the box
owner's routes on loopback, so check the box's repositories and code-host
connections.

## Box host keys leaked {#host-keys}

`ligus remote reset` makes new host keys and a new host id; every tunnel
phone must pair again. If the relay uses a tunnel allowlist, replace the old
host id with the new one in `LANGSTROTH_RELAY_TUNNEL_HOSTS`.

## After any incident {#after}

- Check each device's guard log (the `guard_signals` table in `ligus.db`) for
  ingest rejections around the time of the incident.
- Consider turning on [dearth mode](../../ligus/features/guards.md#dearth-mode) while
  you clean up: new members and devices then need two Admins.

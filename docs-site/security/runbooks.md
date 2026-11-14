# Security runbooks

Step-by-step responses for the incidents you're most likely to have. Each
says what the step achieves and what it doesn't. Background:
[Security model](index.md), [Threat model](threat-model.md).

!!! info "Two different levers"
    **Revoking** a device or removing a member stops their **new events**
    from being accepted. **Rotating** the workspace key stops them from
    **reading new traffic**. You usually need both. In 2.0 the only way to
    rotate is **Remove & revoke** on a member (People pane), which seals a
    new key to every remaining *desktop*; there is no stand-alone "rotate
    key" button, and headless devices can't follow a rotation (see
    [Known limitations](threat-model.md#known-limitations)). When rotation
    isn't available or isn't enough, move to a new workspace
    ([below](#move-to-a-new-workspace)).

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
4. **If you aren't an Admin** of the workspace: device revocation is an
   Admin-level change on the workspace log (`min_role_for` in
   `crates/hive-core/src/authorization.rs`), so other devices may not apply
   a revocation you sign. Ask an Admin to confirm the device shows as revoked
   on their side, and if it doesn't, to remove that device's member (for a
   box that was its own member) or to treat it as a
   [compromised device](#compromised-device).
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
  direct path.

A phone paired **only** on the direct path has no entry in
`ligus phones list`. Rotate the token by hand; a running `ligus serve`
picks up the new file within 2 seconds:

```sh
umask 077
openssl rand -hex 32 > "$LIGUS_DATA_DIR/serve.token"
```

Then re-pair your remaining phones. If the phone app's Face ID lock was on,
the thief also needed your face or passcode; revoke anyway.

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
3. **Cut its reads.** It holds the workspace key. Removing a member with
   **Remove & revoke** rotates the key for remaining desktops. For your own
   lost device, or any workspace with headless devices,
   [move to a new workspace](#move-to-a-new-workspace).
4. **Rotate what it held besides Ligus keys**: model API keys, MCP tokens,
   `LIGUS_WS_SECRET_*` values, the `gh` login of a serve box, Git
   credentials.
5. A compromised `ligus serve` also held phone credentials:
   `ligus remote reset` on its replacement gives new host keys; pair phones
   again.

## Offboarding a person {#offboarding}

1. **Admin, People pane → Members → Remove & revoke** on the person. This
   removes them from the roster and rotates the workspace key, sealed to the
   remaining members' desktops. The toast says whether the key was rotated
   ("No relay configured, so the key wasn't rotated" means it wasn't).
2. If the relay enforces membership, also remove them in **Settings → Team
   sync → Team members** (or the relay roster). That alone doesn't rotate
   the key; step 1 does.
3. Revoke their relay token ([below](#relay-token)).
4. If they had access to shared worker or serve boxes, remove their logins
   there and rotate any secrets they could read.
5. **If the workspace has `ligus serve` or `ligus worker` devices**, the
   rotation strands them (they keep the old key and can't read new desktop
   traffic, and what they write stays readable by the removed person). Move
   to a new workspace.
6. Remember: they keep everything they had already synced.

## Move to a new workspace {#move-to-a-new-workspace}

The complete way to cut someone's reading, including on headless devices.

1. **Add workspace → Create** on an Admin's desktop. It gets a new random
   room and key. **Copy invite**.
2. Re-create the workspace configuration there (agents, runtimes, skills,
   channels). `ligus config export` / `ligus config import` moves workspace
   runtimes (inline secrets are exported as `secretRef`s).
3. Invite the people who stay (by GitHub handle, or the invite over a
   trusted channel).
4. Re-enroll headless boxes with **new** data dirs:
   `ligus enroll 'hivews1:…'`, then restart them.
5. Archive the old workspace. Its history stays on the devices that had it.

## Rotate the workspace key {#rotate-workspace-key}

There is no direct rotation command in 2.0. Rotation happens when an Admin
uses **Remove & revoke**, invites someone by GitHub handle, or opens a DM.
To rotate for any other reason, [move to a new workspace](#move-to-a-new-workspace).

## Relay token compromised {#relay-token}

| Gate | Revoke |
|---|---|
| `LIGUS_RELAY_ACCESS_TOKENS` | remove the token from the list and restart the relay |
| Signed `hrt1` tokens | the open-source relay has no per-token revocation list: tokens stay valid until they expire. Issue them with `--exp-days`. To cut one off now, generate a new issuer key (`ligus-relay keygen`), set the new public key, restart, and re-issue tokens to everyone else |
| Relay with an admin API (downstream builds) | revoke the token there; takes effect immediately |

A relay token only grants use of the relay. Without the workspace key it
reads nothing.

## Issuer key leaked {#issuer-key}

Anyone with the issuer seed can mint relay tokens. Generate a new pair, set
`LIGUS_RELAY_TOKEN_PUBKEY` to the new public key, restart, re-issue tokens.
Remove the old seed from **Settings → Team sync → Advanced → Relay issuer
key** on any desktop that held it.

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

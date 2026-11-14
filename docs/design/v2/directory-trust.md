# Relay-directory trust for invites and DMs

Status: Ligus 2.0, Track B residual 7. Part implemented (verification against
signed key publications this device already holds, plus an explicit
"unverified recipient" confirmation). Signed directory records are designed
here and deferred: they need a relay change.

## The gap

Two flows seal secrets to key-agreement keys that the relay directory returns
for a GitHub handle (`GET /v1/directory/<login>`):

- **Invite by handle** (`invite_by_handle`) seals the workspace invite code,
  which carries the passphrase and every authorized epoch key, to each device
  in the invitee's directory entry.
- **Open a DM** (`friend_open_dm`) seals the DM's connection code (its key) to
  the friend's directory devices.

The directory entry is `{githubId, login, name, devices: [{deviceId,
kaPublic}]}`, written by `POST /v1/directory/register {device_id,
ka_public}`. Nothing in it is signed by the invitee. A hostile or compromised
relay can add its own device (or swap a key) and read the whole workspace
history. Rotations no longer use directory keys (B1: recipients come from
`DeviceKeyAgreementPublished` on the config log), so this is the remaining
place where the relay is trusted with key material.

## What 2.0 does

1. **Signed form we already have.** A device's key-agreement key is published
   on a workspace config log as `DeviceKeyAgreementPublished { device_id,
   ka_public }`, signed by that device (`hive-envelope-v3`), whose
   certificate is signed by the account key (`hive-device-cert-v1`), and the
   fold only folds it when the device is certified and not revoked. No new
   domain string or wire id.
2. **Verify before sealing.** Before sealing, the desktop collects, from every
   workspace config log it holds with a founder pin, the key-agreement keys
   published by non-revoked devices certified for the invitee's account
   (`account_id_for(github_id)`). A directory device whose key is in that set
   is **verified**.
3. **Seal only to verified devices when any exist.** If at least one of the
   invitee's directory devices is verified, the code is sealed to the
   verified ones only. Their other devices get the workspace key through the
   next rotation, once they publish on the config log.
4. **Otherwise, confirm.** If none is verified (first contact, the usual case
   for a DM), the command refuses with an `unverified recipient` error that
   names the handle and the number of devices. The UI shows a warning that
   the relay vouches for these keys and nothing else does, and seals only if
   the inviter confirms. Confirming is a per-action decision; nothing is
   remembered.

What this buys: anyone the inviter already shares a workspace with is
protected against key substitution by the relay. For strangers the risk is
now visible and explicit instead of silent.

What it doesn't: the account key binding inside a shared workspace is itself
trust on first use per workspace (first `AccountKeyRegistered` wins, a
conflicting one makes the account disputed). A relay that could already
inject identity into a shared workspace before the invitee joined it could
also make its key look verified. The relay also maps handle to `githubId`; a
relay that maps a handle to a different known account makes the code go to
that account (the UI shows the login and name it resolved).

## Signed directory records (deferred to relay v0.3 / 2.x)

Design, for when the relay can store an opaque record per device:

- **Record.** The invitee's device publishes, alongside `ka_public`, an
  opaque `record` field: its `DeviceKeyAgreementPublished` envelope (any
  workspace id; the reader checks only the signature chain and the
  `ka_public` match), its `DeviceCertificate`, and its account's
  `AccountKeyRegistered` public key. Reusing these forms means no new domain
  string: the chain is `hive-device-cert-v1` (account key over device key)
  then `hive-envelope-v3` (device key over the publication).
- **Anchor.** The account key must be anchored independently of the relay:
  GitHub's `ssh_signing_keys` for the login (the Option C path in
  `identity_verifier.rs`, already parsing OpenSSH Ed25519 keys), or a key
  pinned from an earlier verified contact. Without an anchor the record only
  proves self-consistency, which a relay can fake, so it would still show as
  unverified.
- **Wire.** `POST /v1/directory/register` gains an optional `record` (skipped
  when empty) and `GET /v1/directory/<login>` returns it per device. An older
  relay drops the field, and clients fall back to the 2.0 behaviour above.
  This is a `/v1` body change, so it waits for the relay repo and a version
  bump; the 2.0 ground rules freeze `/v1` bodies.

## Tests

- `keyring_tests::directory_keys_verified_only_from_a_certified_devices_publication`
  (hive-runtime): a key published on a pinned config log by a device
  certified for the account is verified; a key nobody published (relay
  substitution), another account's key, a key from an unpinned fold and a
  revoked device's key are not.
- `keyring_tests::directory_seal_plan_prefers_verified_devices`: with a
  verified device, only verified devices are sealed to (the relay's extra
  device is left out); with none, sealing needs the inviter's confirmation.
- `web/src/lib/confirm.test.ts` (`withUnverifiedRecipientConfirm`): the UI
  asks before sealing to unverified keys, seals only after a confirm, and
  seals nothing on cancel.

## Code

- `crates/hive-runtime/src/directory_trust.rs`: `signed_ka_keys`, `plan`.
- `app/src/lib.rs`: `seal_plan`, used by `invite_by_handle` and
  `friend_open_dm` (both take `confirm_unverified`; the DM decides before it
  creates anything, so a refusal leaves no half-made DM).
- `web/src/lib/confirm.ts`: `withUnverifiedRecipientConfirm`, used by the
  People panel's invite and Friends' Message.

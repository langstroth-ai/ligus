# Identity link statement

The exact bytes a [linked account](../features/linked-identities.md) signs,
the signature format, and the events that carry it.

## Statement

UTF-8, fields in this order, each line ending in one LF (`\n`), no trailing
whitespace, exactly one LF at the end:

```text
ligus-identity-link-v1
account: 6f1c2d3e-0000-4000-8000-000000000001
account-key: ed25519:BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc
workspace: 11111111-1111-4111-8111-111111111111
provider: github
host: github.com
subject: 1234567
handle: alice
issued: 2026-11-14T12:00:00Z
expires: 2027-11-14T12:00:00Z
nonce: AAECAwQFBgcICQoLDA0ODw
```

| Field | Value |
|---|---|
| `account` | the Ligus account id, lowercase hyphenated |
| `account-key` | `ed25519:` + base64url (no padding) of the 32-byte account signing key in force where the link is published |
| `workspace` | the workspace the link is published in (a contact card's: its contact workspace). A statement is valid in that workspace only |
| `provider` | `github`, `gitlab`, `forgejo`, `gitea`, `sourcehut`, `tangled` |
| `host` | the forge host (`github.com`, `gitlab.example.com`, `codeberg.org`, `meta.sr.ht`), or `atproto` for Tangled |
| `subject` | the stable id: numeric user id, `~user` (SourceHut), or a DID (Tangled) |
| `handle` | the handle at link time, for display only; never used for lookup |
| `issued`, `expires` | RFC 3339 UTC at whole seconds; at most two years apart |
| `nonce` | base64url of 16 random bytes |

No field may contain a line break or control character. A verifier rebuilds
these bytes from the link's fields, the workspace and the account key, and
requires byte equality before it checks the signature.

## Signature

An OpenSSH [SSHSIG](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.sshsig)
over the statement with:

- namespace **`ligus-identity-link-v1@ligus.ai`** (length-prefixed in the
  SSHSIG blob, so it carries no `\0` terminator);
- hash `sha512`.

```sh
ssh-keygen -Y sign -n ligus-identity-link-v1@ligus.ai -f ~/.ssh/id_ed25519 < statement.txt > statement.sig
```

`-f` may name a public key whose private half is in your ssh-agent.

### Checking one by hand

```sh
echo "alice $(cat id_ed25519.pub)" > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I alice \
  -n ligus-identity-link-v1@ligus.ai -s statement.sig < statement.txt
```

then confirm the forge lists that key for the subject id (for GitHub:
`GET https://api.github.com/user/1234567`, then
`/users/{login}/keys` or `/users/{login}/ssh_signing_keys`).

## Events

Two config-log events (frozen in 2.0; an older decoder folds them as
`unknown`):

```json
{ "kind": "identityLinkPublished",
  "link": { "linkId": "…", "accountId": "…", "provider": "github", "host": "github.com",
            "subjectId": "1234567", "handleAtLink": "alice", "method": "sshSig",
            "statement": "ligus-identity-link-v1\n…", "proof": "-----BEGIN SSH SIGNATURE-----…",
            "keyFingerprint": "SHA256:…", "nonce": "…",
            "issuedAt": "2026-11-14T12:00:00Z", "expiresAt": "2027-11-14T12:00:00Z" } }

{ "kind": "identityLinkRemoved", "accountId": "…", "linkId": "…" }
```

`statement`, `proof`, `keyFingerprint` and `nonce` are left out when empty
(the `keyPublished` method has none). `method` is `sshSig` or `keyPublished`
in 2.0; `atprotoRecord`, `oidcAttestation` and `selfAsserted` are reserved,
and a method a build doesn't know folds as a claim it shows as "not checked".

What the fold enforces (every device, offline, no network):

- On the workspace config log only.
- Published by the account itself: the signer's certified account equals
  `link.accountId`, from a certified device that holds the account (not a
  delegated host), by a person (never an agent, never a runtime effect), as
  a member at any role.
- The statement equals the rendering above for this workspace and the
  account key in force at the event's position.
- At most 8 KiB, at most 16 live links per account; a new link for the same
  `(provider, host, subject)` replaces the old one.
- Removal: the account itself, or an Admin who isn't below the account's
  member.

The SSHSIG and the forge are checked device-locally, never in the fold; the
result is cached on the device for 24 hours and never synced.

## Contact cards

A link marked "on my contact card" rides this device's contact record as an
`identityLinkPublished` envelope signed by one of the record's devices, with
its statement bound to the contact's workspace. A reader drops any link that
isn't (the contact still verifies).

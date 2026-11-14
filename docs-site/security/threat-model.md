# Threat model

What Ligus defends against, what it doesn't, and the limitations you should
plan around. Read [Security model](index.md) first for the components and
trust boundaries.

## Assets {#assets}

1. **Workspace content**: messages, proposals, diffs, attachments, workflow
   definitions, inline runtime credentials.
2. **Keys**: account and device signing keys, workspace keys, key-agreement
   keys, `serve.token`, phone credentials.
3. **Your machines and repositories**: what agents can execute or change.
4. **Credentials agents use**: model API keys, MCP tokens, the `gh` login of
   a `ligus serve` host.

## Attackers {#attackers}

### The relay {#the-relay}

A relay operator (or anyone who compromises the relay) **can**:

- see metadata: workspace ids, envelope counts, sizes and timing, IP
  addresses, device public keys and labels, GitHub logins that use the
  directory, which tunnel hosts exist and when phones connect;
- drop, delay or replay ciphertext, or refuse service;
- with membership mode on, see the roster and invite metadata.

It **can't** read content, forge a member's events (they are signed, and
roles are checked by every device), or obtain a workspace key: keys are
exchanged out of band and rotations are sealed to devices. In the tunnel it
can't read or alter phone traffic (Noise, with the box's key pinned from the
QR code).

`LIGUS_IDENTITY_MODE=github-signing-keys` additionally checks each account
key against the GitHub login's SSH signing keys, so a hostile relay can't
substitute an account key in the directory.

### A network attacker {#network}

Between devices and the relay: TLS (you must put TLS in front of the relay).
Between phone and box on the direct path: **plain HTTP with a bearer token**;
it's only safe on loopback, a trusted LAN, or a VPN like Tailscale. The bind
guard refuses public addresses for that reason.

### A malicious or compromised member {#member}

A member holds the workspace key, so they **can read everything in the
workspace**, including history from before they joined and any inline
runtime credentials (use `secretRef` to keep keys off the log). What they
**can't** do is act above their role: changes their role doesn't allow are
dropped on every device; they can't promote themselves, approve their own
join in dearth mode, vote as someone else, or forge the founder. After
removal with key rotation they can't read new desktop traffic (see the
headless caveat in [Known limitations](#known-limitations)).

### Prompt injection and hostile content {#prompt-injection}

Content an agent reads (a web page, an MCP result, a file in an untrusted
repo, a teammate's message) can try to steer it. Defences:

- edits land in an isolated worktree and come back as proposals; nothing
  reaches your working tree without a human **Implement** (or an explicit
  `LIGUS_SERVE_APPLY_ON_APPROVE` on a serve box);
- MCP results are marked untrusted, and once a turn has read project files
  MCP calls are refused (exfiltration guard);
- chat lockdown trips on injection markers, credentials in diffs and
  unrequested CI changes;
- dearth mode narrows egress and requires human approval for CI and deploy
  paths;
- secret redaction masks credentials and invite codes in replies before
  they're logged;
- on headless hosts, tools only run for authors at or above
  `LIGUS_MCP_MIN_ROLE`.

These reduce risk; they don't make an agent safe to run with broad
permissions on an untrusted repository. Runtimes Ligus can't confine from
outside (aider, pi, Hermes, Antigravity, Copilot) are refused whenever
egress is restricted, but run with their own permissions otherwise.

### Someone with your phone {#lost-phone}

The phone app holds a credential that lets it act through your box with the
box's role. Optional Face ID / passcode lock protects the app on an unlocked
phone. Revoke the phone from the box; see
[Lost phone](runbooks.md#lost-phone).

### Someone with a copy of a data dir {#data-dir}

A data dir (desktop or CLI) contains private key seeds, workspace keys,
tokens and every event in clear. Whoever has it **is** that device. Disk
encryption and OS account separation are the protection; see
[Compromised device](runbooks.md#compromised-device).

## Known limitations {#known-limitations}

Stated plainly, as of 2.0.0:

1. **Key seeds are plain files.** Account and device seeds are raw 32-byte
   files under `keys/`, written with the process's default umask (often
   world-readable `0644` on Linux). No OS keystore is used. Keep data dirs
   `0700` (`chmod -R go-rwx <data dir>`); the shipped `ligus-serve.service`
   sets `UMask=0077`.
2. **`config.toml` and `settings.json` hold the workspace key in clear.**
   So does a `hivews1:` invite. Treat each like a password.
3. **Key rotation doesn't reach headless devices.** `ligus serve` and
   `ligus worker` only know the base (epoch 0) key from the invite; they
   can't open rotated keys. After a **Remove & revoke**, desktops seal new
   traffic under the new key, which headless boxes can't read, while
   anything a headless box writes is still sealed under the old key, which
   the removed member holds. For a workspace with headless devices, offboard
   by moving to a new workspace; see
   [Offboarding](runbooks.md#offboarding).
4. **Invite codes carry the base key only.** A joiner who arrives with a
   `hivews1:` code after a rotation needs the rotation sealed to their
   devices (inviting by GitHub handle does this).
5. **No account-key rotation.** The core supports rotating an account key,
   but nothing in the app or CLI exposes it. A leaked account key means a new
   account.
6. **Removal is not erasure.** A removed member keeps everything they already
   synced. Rotation protects only what comes after.
7. **Roles don't limit reading.** Every member can read the whole workspace.
8. **The relay's open mode is open.** Without a token gate, anyone who knows
   the URL can store ciphertext on it and read ciphertext of workspaces whose
   ids they know. A malformed `LIGUS_RELAY_TOKEN_PUBKEY` falls back to open
   (see [Self-hosting a relay](../networking/self-host.md#entitlement-who-may-use-the-relay-at-all)).
9. **Relay per-IP limits trust `Fly-Client-IP`.** Off Fly, strip it at the
   proxy.
10. **The relay sees metadata** (above), and the relay's admin API isn't in
    the open-source build, so per-person relay tokens need signed tokens or a
    restart to change an allowlist.
11. **The direct phone path is plain HTTP.** Use Tailscale or a LAN you
    trust, or the relay tunnel.
12. **One `serve.token` for every direct-path client.** Revoking one direct
    phone means rotating it for all. Tunnel phones are revoked one by one.
13. **A `ligus serve` box acts with its own role.** With
    `LIGUS_SERVE_APPLY_ON_APPROVE=1`, approving from the phone also applies
    the change on the box.
14. **Desktop runtimes run as you.** Claude Code and other CLI agents run
    with your OS permissions within their own permission mode; isolation
    is a git worktree, not a sandbox. Kubernetes executors are the
    sandboxed path, and their controller isn't built yet.
15. **Revoking a device doesn't rotate the key.** A revoked device's new
    events are refused, but it still holds every key it had. Rotation happens
    only through **Remove & revoke** on a member (and a few invite paths); for
    your own lost device, move to a new workspace. See
    [Runbooks](runbooks.md#compromised-device).
16. **In default identity mode, events from a device with no certificate are
    accepted**; dearth mode binds where certificates matter. See
    [Dearth mode](../concepts/security.md#dearth-mode-a-tighter-entrance-on-demand).

## Out of scope {#out-of-scope}

- A compromised OS account or machine that runs Ligus.
- Model providers' and MCP servers' handling of what turns send them.
- Availability against a determined denial of service on your relay.

## Reviewer findings {#reviewer-findings}

Security review findings that add to or change this threat model are listed
here with their status.

*None recorded yet.*

# Security Policy

Ligus handles end-to-end-encrypted workspaces, device keypairs, and a relay that
forwards ciphertext. We take vulnerabilities seriously and appreciate coordinated
disclosure.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report it privately through GitHub's private vulnerability reporting: open the
repository's **Security** tab and choose **Report a vulnerability**
([direct link](https://github.com/langstroth-ai/ligus/security/advisories/new)).
The discussion stays attached to the fix.

If you can't use GitHub, email **support@langstroth.ai** instead.

Please include: affected component (app / relay / crypto / MCP / sync), version
or commit, reproduction steps or a PoC, and impact. We aim to acknowledge within
a few days and will keep you updated on the fix and disclosure timeline.

## Scope

In scope:

- **`ligus-core` crypto & signed envelopes** — Ed25519 identity, X25519 key
  agreement, ChaCha20-Poly1305, HPKE per-device sealing, envelope
  verification/quarantine.
- **The relay** ([github.com/langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay))
  — access-token gating, content-blind forwarding (it must never be able to read
  plaintext), the `hrt1` token format.
- **Sync / P2P** — rendezvous, STUN/hole-punch, TURN fallback, workspace-key
  rotation.
- **MCP & tool execution** — the install-is-inert-until-enabled gate, per-tool
  trust, subprocess bridges.
- **Tauri IPC / desktop app** boundaries.

Out of scope: vulnerabilities in third-party model providers or MCP servers you
choose to run; social-engineering; issues requiring an already-compromised device.

## Supported versions

Only the newest release is supported. Please test against it (or the latest
`main`) before reporting.

**Policy:** security fixes ship inside the next release; there are no patch
releases of older versions. Fixes found before 0.1.0 are bundled into 0.1.0, the first Ligus release.

## Handling

Valid reports are fixed in the next release, with a GitHub Security Advisory and credit to
the reporter (unless you prefer to remain anonymous). Because the desktop client
is a single OSS binary and the relay is self-hostable, please note in your report
whether the issue affects self-hosted deployments so we can advise operators.

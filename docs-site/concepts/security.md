# Security & trust

Ligus is end-to-end encrypted and the relay is content-blind. This page states
the guarantees you can rely on when a workspace syncs through a relay. A
local-only workspace (no relay) puts nothing on the wire and is unaffected by
everything below.

## E2EE is mandatory for relay sync

Ligus **refuses to push plaintext to a relay**. A workspace configured for sync
but with no workspace key does not sync — the app surfaces a **Sync error**
telling you to configure a key. The relay therefore only ever holds sealed
(E2EE) bodies; there is no "encrypt later" mode that leaks cleartext in the
meantime.

## The workspace key never transits the relay

The key is exchanged **out-of-band**:

- The `hivews1:` invite code carries the key *inside the code itself*, so share
  the code over a channel you trust — paste it directly to the joiner. See the
  [invite code format](../reference/invite-payload.md).
- [Invite by GitHub handle](../features/invites.md#invite-by-github-handle)
  seals the key to the invitee's devices instead, so nothing sensitive is
  copy-pasted — the key still never touches the relay.

The old relay-brokered "short code" that published the key to the relay's
pairing store has been **removed**; the relay is never a party to key exchange.

## The relay is content-blind

Every envelope is sealed with ChaCha20-Poly1305 *before* it leaves the device;
the relay stores and forwards ciphertext only. It cannot read your traffic, and
because it never holds the key it cannot help you recover a lost one.

## Authorization is enforced on ingest, not just at authoring

Governance is re-checked when a synced event is **projected**, not only when it
is authored. A membership change, a `MemberRoleChanged`, or a workspace-config
change from an author who lacked the required role is **dropped during
projection** — deterministically, on every device, so convergence is preserved.

A malicious member therefore cannot forge a validly-signed
`MemberRoleChanged{ self: Viewer → Owner }` to seize the workspace: the
signature verifies, but the author's role at that point did not permit the
change, so every peer drops it. Membership is workspace-wide (via the config
log) and roles are **Owner / Admin / Contributor / Viewer**.

The workspace **founder is pinned via the invite**: the invite carries the
founding identity, and projection only accepts the real founder's seed as
the base snapshot. So a malicious keyholder can't forge a competing base to
hijack or fork the workspace out from under everyone else.

## The sandbox envelope around every agent turn

The workspace, not each runtime, decides how far an agent turn may reach, and
the same rules apply whether the turn runs on Claude Code, Codex, aider, pi or
an API model calling MCP tools:

- **Isolated worktree.** An agent that can edit files runs in its own git
  worktree, and its changes come back as a proposal for review. While the
  envelope is tightened (dearth mode, a chat lockdown), a turn that can't get
  a worktree is refused instead of running in place.
- **Filesystem allow-list.** Agents never write into `.git`, `.ligus`,
  `.claude`, `.vscode` or the workspace config files.
- **Egress policy.** Normally open (each runtime's own permission tier
  applies). Tightened, it is either *allow-listed MCP servers only* or
  *closed*. Ligus enforces it per runtime: Claude Code loses its web and shell
  tools and every MCP server outside the list; Codex runs with network access
  off; API models only see allowed MCP servers and never `run_command`.
  Runtimes Ligus can't confine from outside (aider, pi, Hermes, Antigravity,
  Copilot) are **refused** while egress is restricted, with a note in the chat.
- **Secret redaction.** API keys, tokens, private keys and Ligus invite codes
  in an agent's reply are masked before the reply is written to the log, so
  they never reach other members or the relay.

On Kubernetes the same envelope becomes the turn executor's pod spec and
NetworkPolicy.

## Dearth mode: a tighter entrance on demand

An Admin can put the workspace in [dearth mode](../features/guards.md#dearth-mode)
when it is under pressure. New members then need two different Admins, a
newly certified device needs an Admin of another account to approve it before
its certificate counts, agents keep only allow-listed MCP servers, and changes
to protected paths need a human member's own approval. The switch, the join
approvals and the votes that satisfy a human gate are all re-checked on
ingest: a vote counts toward a human gate only when the voter it names is the
person who signed it, and an approval carried inside a proposal snapshot never
counts.

Residual: in the default identity mode, events from a device with no
certificate at all are still accepted (as before dearth mode); dearth mode
withholds certificates, so it binds where certificates matter (owner-device
enrollment, the `GithubSigningKeys` identity mode, impersonation checks).

## What this page does not cover

Private key seeds live as files on disk, not in an OS keystore — see
[Identity & devices](identity.md#key-storage) for what protects them and
[Self-hosting a relay](../networking/self-host.md) for connection-level access
control.

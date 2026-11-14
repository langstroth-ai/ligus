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
meantime. The rule holds inbound too: a device that holds the workspace key
never applies an unsealed body from the relay, so a relay can't inject
events into an encrypted workspace. (Plaintext history a workspace left on a
relay before it had a key therefore doesn't appear on devices that fetch it
later.)

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

Proposal-set steps ride a chat's own log, so they are checked against the
workspace's member list from the config log instead: a pre-flight result, a
change to the dial, a quorum or stop signal, or a decision signed by someone
below Contributor (or below the set's decision role), or by a non-member, is
dropped on every device.

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
- **Filesystem allow-list.** Agents never write into `.git`, `.ligus`, `.hive`,
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
- **No repository code run by Ligus.** Ligus's own git calls (worktrees,
  diffs, apply, the hygiene sweep) never run the repository's hooks or
  fsmonitor, and while egress is restricted Claude Code loads no settings or
  hooks from the repository, only your user settings.
- **Ligus's secrets stay out of child processes.** Agent CLIs, MCP servers,
  pre-flight tests and git start without the workspace key, relay and tunnel
  tokens or `LIGUS_WS_SECRET_*` values in their environment. Model provider
  keys pass through, since the runtimes need them.
- **No remote images in chat.** Markdown images from remote URLs render as
  links, so a reply can't send data to a server just by being displayed.

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

While dearth mode is on, every device holds, in every identity mode, the
events of a device still waiting for its second approval and any event no
certified device vouches for. Held events aren't dropped: sync waits on
them and judges them again, so they apply once the device is approved. The
protected-path gate
reads the paths in the proposal's diff itself, not the list the author
declared, and matches them case-insensitively; a human approval stops
counting if the patch changes afterwards.

Residual: outside dearth mode, the default identity mode still accepts events
from a device with no certificate.

## Self-medication: a chat that shows infection locks itself down

When an agent turn reads a prompt-injection marker in a tool result, adds a
credential to a diff, or changes CI configuration nobody asked about, Ligus
[locks that chat down](../features/guards.md#chat-lockdown): no network or
MCP access for its agents and a human approval on every proposal, until a
human member clears it. The lock is an event on the workspace config log, so
every device applies it; clearing it is re-checked on ingest (a non-member,
Viewer or agent account can't).

## What this page does not cover

The full [security model](../security/index.md), the
[threat model and known limitations](../security/threat-model.md), and the
[incident runbooks](../security/runbooks.md) have their own section.

Private key seeds live as files on disk, not in an OS keystore — see
[Identity & devices](identity.md#key-storage) for what protects them and
[Self-hosting a relay](../networking/self-host.md) for connection-level access
control.

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

### One engine, judged at each event's position {#authz-at-position}

Every event a device folds, and every event it authors, goes through the
same authorization engine, so authoring and folding can't disagree:

- **Roles as of the event.** Each chat event stamps the position of the
  workspace config log its author had seen. Proposal-set steps, votes and
  applies are judged against the roster **at that position**, never today's:
  demoting an Admin later doesn't undo their decisions, and promoting
  someone later doesn't validate steps they took before. An event whose
  position hasn't synced to this device yet **waits** (it is kept, left out
  of the chat, and blocks side effects on the proposal it touches) and is
  decided once it can be.
- **Members only.** Every author must be a workspace member at the event's
  position. Joining needs a single-use invite an Admin issued; a removed
  account also needs an Admin to re-add it.
- **Votes that mean something.** A vote counts only from the voter it
  names, at their role then (the role a vote claims is ignored), and only
  for the content digest it names; editing a proposal's patch voids earlier
  votes. Proposal status is derived from counted votes, and *Applied* comes
  only from a human member's apply event for the approved content. A vote
  signed by a `ligus serve` box enrolled as its owner's device doesn't count
  (see [Phone and serve approvals](../security/index.md#roles)).
- **Agents.** An agent can sign with its own key, certified by the host it
  is bound to and retired when it moves; such events are judged by a capped
  Agent tier that never votes, applies or administers.
- **Versioned rules.** The rules are versioned on the config log; a version
  this build doesn't know makes the workspace read-only on this device.
- **Bytes as received.** Envelopes are verified, stored and re-sent as the
  bytes received, so an event from a newer build keeps fields this build
  doesn't know and still verifies everywhere.
- **No environment.** Nothing that decides which events fold reads the
  environment; `LIGUS_IDENTITY_MODE` is ignored.

The workspace **founder is pinned via the invite**: the invite carries the
founding identity and a fingerprint of the founder's account key, a joining
device pins both, and projection only accepts the real founder's seed as the
base snapshot. So a malicious keyholder can't forge a competing base, or a
second key for the founder, to hijack or fork the workspace out from under
everyone else. Trust rosters are kept per workspace: a removal in one
workspace doesn't touch devices in another.

## The sandbox envelope around every agent turn

The workspace, not each runtime, decides how far an agent turn may reach, and
the same rules apply whether the turn runs on Claude Code, Codex, aider, pi or
an API model calling MCP tools:

- **Isolated worktree.** An agent that can edit files runs in its own git
  worktree, and its changes come back as a proposal for review. While the
  envelope is tightened (dearth mode, a chat lockdown), a turn that can't get
  a worktree is refused instead of running in place.
- **Filesystem allow-list.** Agents never write into `.git`, `.ligus`, `.hive`,
  `.claude`, `.vscode` or the workspace config files. Ligus's built-in file
  tools and the OS sandbox (below) both read this list from the envelope.
- **Egress policy.** Normally open (each runtime's own permission tier
  applies). Tightened, it is either *allow-listed MCP servers only* or
  *closed*. Ligus enforces it per runtime: Claude Code loses its web and shell
  tools and every MCP server outside the list; Codex runs with network access
  off; API models only see allowed MCP servers and never `run_command`.
  Runtimes Ligus can't confine from outside (aider, pi, Hermes, Antigravity,
  Copilot) are **refused** while egress is restricted, with a note in the chat.
- **Secret redaction.** API keys, tokens, private keys and Ligus invite codes
  in an agent's reply are masked before the reply is written to the log, so
  they never reach other members or the relay. Ligus's **own** secrets are
  masked by exact value as `[ligus-secret]`: the serve token, relay `hrt1`
  tokens, phone pairing credentials, device and workspace key seeds, and
  every key or token in this device's settings. That covers streamed
  replies and proposal titles, bodies and diffs.
- **No repository code run by Ligus.** Ligus's own git calls (worktrees,
  diffs, apply, the hygiene sweep) never run the repository's hooks or
  fsmonitor, and while egress is restricted Claude Code loads no settings or
  hooks from the repository, only your user settings.
- **An allow-listed environment.** Agent CLIs, MCP servers, `run_command`
  and pre-flight tests start with an empty environment plus an allow-list:
  `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TZ`, `TERM`,
  `TMPDIR`, the CA variables, `XDG_*` on Linux, the CLIs' own config-dir
  variables when you set them, the proxy variables Ligus sets, git
  attribution, and **one** provider key: the one you configured for that
  runtime in Ligus. A key in Ligus's own environment (`ANTHROPIC_API_KEY`,
  `OPENAI_API_KEY`, …) is no longer inherited; Ligus still uses it for its
  own API calls. Claude Code therefore uses its subscription login unless
  you give its runtime a key in Ligus.
- **No remote images in chat.** Markdown images from remote URLs render as
  links, so a reply can't send data to a server just by being displayed.

On Kubernetes the same envelope becomes the turn executor's pod spec and
NetworkPolicy.

## The OS sandbox around agent processes {#os-sandbox}

Every process Ligus starts for a turn (a CLI agent, a stdio MCP server,
`run_command`, a pre-flight test) runs inside an OS sandbox planned from the
envelope, the runtime and the author gate. The plan is the same on the
desktop, `ligus serve`, `ligus worker` and `ligus agent`.

| | macOS (Seatbelt, `sandbox-exec`) | Linux (bubblewrap + Landlock + seccomp) |
|---|---|---|
| Writes | the turn's worktree (only when the turn may write), its scratch dir, the runtime's own state dirs (`~/.claude`, `~/.codex`, `~/.copilot`, …) | the same, as bind mounts on a read-only `/`; Landlock limits writes to them again |
| Never readable | Ligus's data dir (keys, `settings.json`, the database, `serve.token`), a `--token-file` elsewhere, `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config/gcloud`, `~/.kube`, `~/.netrc`, `~/.git-credentials` | the same (tmpfs over them), plus `$XDG_RUNTIME_DIR`, so the D-Bus session bus and Secret Service are out of reach |
| Read-only inside the worktree | `.git`, the protected dirs, `ligus.config.toml`, `hive.config.toml`, `.envrc` | the same |
| Network | denied except the turn's egress proxy on loopback | a fresh network namespace; the only way out is the egress proxy through a socket |
| Other | | seccomp denies `ptrace`, `process_vm_*`, `mount`, `bpf`, `keyctl`, kernel-module and `kexec` calls, and new user namespaces |

- **cwd and HOME.** An agent never runs in your home directory, in `/` or in
  Ligus's data dir; a turn without a worktree gets a fresh scratch dir
  (`$TMPDIR/ligus-turn-…`).
- **Egress proxy.** Each turn gets its own HTTP `CONNECT` proxy on
  `127.0.0.1`, with a random credential in the child's `HTTPS_PROXY`. It
  allows exactly the runtime's model endpoints (Claude Code:
  `api.anthropic.com`, `console.anthropic.com`, `claude.ai`; Codex:
  `api.openai.com`, `chatgpt.com`, `auth.openai.com`; Copilot:
  `api.githubcopilot.com`, `github.com`, `api.github.com`; Antigravity: the
  Google endpoints; an API runtime: its base URL) plus the hosts of the MCP
  servers this chat may reach. Under dearth mode only the allow-listed MCP
  servers' hosts are added; under a chat lockdown only the model endpoints
  remain. The `CONNECT` host and the TLS server name (SNI) must both match;
  there is no TLS interception. A refused endpoint shows as a system note in
  the chat and a content-free decision. A tool that ignores `HTTPS_PROXY`
  simply has no network. To let agents reach more hosts on your device under
  the normal envelope, set `LIGUS_SANDBOX_EGRESS_ALLOW`.
- **Keychain (macOS).** Subscription CLIs (Claude Code, Codex, Copilot,
  Antigravity) keep access to the login keychain, because their logins live
  there; every other agent process can't read the keychain files. Ligus's own
  keychain items are protected by their access lists, not by the profile.
- **Codex** runs its commands inside Ligus's sandbox instead of its own
  (Seatbelt and bubblewrap sandboxes can't nest).
- **No sandbox, no writing agents.** Where neither sandbox is available
  (Windows, a Linux host without bubblewrap or user namespaces,
  `LIGUS_SANDBOX=off`), Ligus still applies the environment allow-list and
  cwd rules, but refuses write-capable agent turns. Read-only turns still
  run.
- **One control among several.** The sandbox does not replace the author
  gate, the MCP role gate, review of proposals or the human gate, and it is
  not a boundary against kernel bugs.

## Who can drive a turn: the author gate {#author-gate}

A turn runs on the host's keys and files on behalf of whoever wrote the
message that triggered it. Every host (desktop, `ligus serve`, `ligus
worker`, `ligus agent`) applies the same gate to every runtime:

- The floor is **Admin** for a turn that can write (a raised permission tier,
  or a CLI that always edits: Codex, aider, pi, Hermes) and **Contributor**
  for a read-only one. `LIGUS_MCP_MIN_ROLE` overrides it; a value below the
  default logs a warning at startup.
- At or above the floor the turn runs as configured. Below it, an API model
  gets no tools, Claude Code, Antigravity and Copilot run at their read-only
  tier (and the OS sandbox mounts the worktree read-only), and Codex, aider,
  pi and Hermes are refused with a note in the chat. An author that can't be
  resolved is below every floor. On the desktop, your own messages always
  clear it.
- The author's role is the one they held **when they posted the message**
  (the roster at the message's position), so a promotion after the fact
  doesn't widen an old message's reach.

## Trust this workspace {#workspace-trust}

A repository can ship runtimes and MCP servers (`ligus.config.toml`,
`.ligus/runtimes.json`, `.ligus/mcp-servers.json`): commands Ligus would run
and endpoints it would send prompts to. They load **inert**. Ligus shows
*"This workspace wants to run N commands (MCP servers, runtimes)"*; **Review**
lists each exact command line and the file it came from, and you enable
items one by one or **Trust this workspace** for all current ones.

- The decision lives only on this device (a table in its database), keyed by
  the repository (its path and first commit) and a digest of each item's
  command, arguments and URL. A changed command is a new item and is inert
  again.
- Items you add in Settings are trusted as you save them.
- Headless hosts use `ligus trust list | enable <digest> | enable-all`.
- The pre-flight test command comes only from `LIGUS_PREFLIGHT_TEST` on your
  device, never from the repository's git config.

## Telemetry stays on your device {#facts}

Per-turn measurements (which agent answered, task class, tool calls, test
results, waits, touched-file keys, route stops) are **facts** on the device
that ran the turn, not events in the signed log. Each host syncs one bounded
summary per agent per day (turn, error and test counts per task class,
proposal outcomes), which is what other devices score from. Routing
decisions stay in the log, because other devices follow them. External
observability tools read facts with `ligus facts export --jsonl` (schema
`ligus.facts.v1`: ids, enums, numbers and timestamps only; any string that
isn't an id or looks like a secret is exported as `redacted`).

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

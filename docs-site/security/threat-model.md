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
exchanged out of band and rotations are sealed to devices. It can't inject
plaintext either: a device that holds a workspace key skips any body from the
relay that isn't sealed. In the tunnel it can't read or alter phone traffic
(Noise, with the box's key pinned from the QR code). Two gaps remain, both in
[Known limitations](#known-limitations): key rotations aren't signed, and the
GitHub-backed routes see the GitHub tokens clients send them.

`LIGUS_IDENTITY_MODE=github-signing-keys` additionally checks each account
key against the GitHub login's SSH signing keys, so a hostile relay can't
substitute an account key in the directory.

### A network attacker {#network}

Between devices and the relay: TLS (you must put TLS in front of the relay).
Between phone and box on the direct path: **plain HTTP with a bearer token**;
it's only safe on loopback, a trusted LAN, or a VPN like Tailscale. The bind
guard refuses public addresses for that reason. Prefer the relay tunnel or a
tailnet address over a LAN bind: on a LAN, anyone on the network can watch
the token go by. See [Securing the box](../ops/serve-box.md#securing-the-box).

### A malicious or compromised member {#member}

A member holds the workspace key, so they **can read everything in the
workspace**, including history from before they joined and any inline
runtime credentials (use `secretRef` to keep keys off the log). What they
**can't** do is act above their role: changes their role doesn't allow are
dropped on every device; they can't promote themselves, approve their own
join in dearth mode, vote as someone else, forge the founder, or revoke
another person's device (that needs Admin, and every device ignores a
revocation or removal from anyone below it). After removal they can't enroll
themselves back: self-enrollment is refused for a removed account until an
Admin re-adds it. After removal with key rotation they can't read new desktop
traffic (see the headless caveat and the unsigned-rotation gap in
[Known limitations](#known-limitations)).

### Prompt injection and hostile content {#prompt-injection}

Content an agent reads (a web page, an MCP result, a file in an untrusted
repo, a teammate's message) can try to steer it. Defences:

- edits land in an isolated worktree and come back as proposals; nothing
  reaches your working tree without a human **Implement** (or an explicit
  `LIGUS_SERVE_APPLY_ON_APPROVE` on a serve box);
- Ligus's own git calls (worktrees, diffs, apply, the hygiene sweep, PR
  fetch and push) never run the repository's hooks or fsmonitor, so a file an
  agent plants can't get code run by Ligus outside the agent's sandbox;
- agent CLIs, MCP servers, `run_command` and pre-flight tests start with an
  allow-listed environment: no Ligus secrets and no provider key except the
  one you configured for that runtime in Ligus;
- agent processes run in the OS sandbox: no reads of Ligus's data or your
  credential dirs, writes only in the worktree, and the network only through
  a per-chat egress allow-list (model endpoints plus the chat's MCP hosts);
- a repository's own runtimes and MCP servers are inert until you trust them
  on your device;
- while egress is restricted, Claude Code loads no settings or hooks from
  the repository (only your user settings), so a cloned `.claude/settings.json`
  can't reopen the network;
- chat markdown never loads remote images: `![](url)` renders as a link you
  choose to open, so a reply can't leak a secret in an image URL just by
  being displayed;
- MCP results are marked untrusted, and once a turn has read project files
  MCP calls are refused (exfiltration guard);
- chat lockdown trips on injection markers, credentials in diffs and
  unrequested CI changes;
- dearth mode narrows egress and requires human approval for CI and deploy
  paths;
- secret redaction masks credentials and invite codes in replies before
  they're logged;
- on every host, a turn runs at its configured tier only for authors at or
  above the floor (Admin for a write-capable turn, Contributor for a
  read-only one; `LIGUS_MCP_MIN_ROLE`). Below it, Claude Code, Antigravity
  and Copilot run read-only and Codex, aider, pi and Hermes are refused with
  a note.

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
tokens and every event in clear. Whoever has it **is** that device. Ligus
writes key seeds `0600` in a `0700` key dir, and `ligus enroll` writes
`config.toml` `0600` in a `0700` data dir, so other local users can't read
them; anything running as your OS user still can, agents included. Disk
encryption and OS account separation are the protection; see
[Compromised device](runbooks.md#compromised-device).

## Known limitations {#known-limitations}

Stated plainly, as of 2.0.0. Items marked **(Phase 0)** are planned for the
coming identity and key refactor; each stays on this list until a release
fixes it.

1. **Key seeds are plain files.** Account and device seeds are raw 32-byte
   files under `keys/`. They are written `0600` in a `0700` directory (and an
   older, looser seed is tightened when it is next loaded), but no OS keystore
   is used: anything running as your OS user can read them.
2. **`config.toml` and `settings.json` hold the workspace key in clear.**
   So does a `hivews1:` invite. Treat each like a password. `ligus enroll`
   writes `config.toml` `0600` and keeps the data dir `0700`.
3. **Agents run as your OS user, inside a sandbox.** Agent processes run in
   the [OS sandbox](../concepts/security.md#os-sandbox) (Seatbelt on macOS,
   bubblewrap + Landlock + seccomp on Linux), which denies the data dir (key
   seeds, workspace keys, `settings.json`, `serve.token`), credential dirs and
   direct network. The sandbox shares your user and kernel: it is not a
   boundary against kernel bugs, and on a host without it (Windows, Linux
   without bubblewrap or user namespaces) the data dir is readable to
   read-only agent turns, while write-capable turns are refused. On Linux a
   protected directory that doesn't exist yet is pinned by an empty
   read-only directory for the turn.
4. **Agents have no identity of their own.** **(Phase 0)** An agent's
   messages and proposals are signed by the device that ran the turn. Peers
   can tell which host ran it, not prove which agent wrote it.
5. **Key rotation doesn't reach headless devices.** **(Phase 0)**
   `ligus serve` and `ligus worker` only know the base (epoch 0) key from the
   invite; they can't open rotated keys. After a **Remove & revoke**,
   desktops seal new traffic under the new key, which headless boxes can't
   read, while anything a headless box writes is still sealed under the old
   key, which the removed member holds. For a workspace with headless
   devices, offboard by moving to a new workspace; see
   [Offboarding](runbooks.md#offboarding).
6. **Key rotations aren't signed.** **(Phase 0)** A rotation is a set of
   sealed copies of the new key with no author signature, and a desktop adopts
   any rotation on the relay it can open, sealing new events under the
   highest epoch it holds. Someone who knows your devices' public
   key-agreement keys (every member sees them, a removed member included) and
   can write the workspace's keyring on the relay can publish a key they
   chose, and your desktop's new traffic becomes readable to them. Until this
   is fixed: gate the relay with tokens, confine tokens to their workspaces
   (`LIGUS_RELAY_WORKSPACE_SCOPE`, see
   [Self-hosting a relay](../networking/self-host.md#per-workspace-token-scope)),
   and after offboarding someone who could reach the relay, prefer
   [moving to a new workspace](runbooks.md#move-to-a-new-workspace).
7. **Proposal-set vote roles are self-declared.** **(Phase 0)** A vote names
   the role it was cast with, and a set's decision floor reads that role
   rather than the voter's role on the roster, so a Viewer's vote that claims
   Admin meets an Admin floor.
8. **Hoisted checks are retroactive.** **(Phase 0)** Proposal-set steps are
   checked against the workspace's *current* member list, not the list at the
   time of each step. Demoting an Admin later undoes the decisions they made;
   promoting someone later validates steps they took before they could.
9. **A `ligus serve` box enrolled as your device votes as you.** **(Phase 0)**
   Votes and approvals sent through its API (from a phone, or from anyone
   holding `serve.token` or a phone credential) count as your own vote. With
   `LIGUS_SERVE_APPLY_ON_APPROVE=1`, approving also applies the change on the
   box. A box that is its own member acts with its own role.
10. **Invite codes carry the base key only.** A joiner who arrives with a
    `hivews1:` code after a rotation needs the rotation sealed to their
    devices (inviting by GitHub handle does this).
11. **No account-key rotation.** The core supports rotating an account key,
    but nothing in the app or CLI exposes it. A leaked account key means a new
    account.
12. **Removal is not erasure.** A removed member keeps everything they already
    synced. Rotation protects only what comes after.
13. **Roles don't limit reading.** Every member can read the whole workspace.
14. **The relay's open mode is open.** Without a token gate, anyone who knows
    the URL can store ciphertext on it and read ciphertext of workspaces whose
    ids they know. On an open relay that keeps state in memory, set
    `LIGUS_RELAY_MAX_STORE_BYTES` so writes can't exhaust its memory.
15. **The relay sees metadata** (above), and the GitHub tokens clients send
    to its directory, account and friends routes (it has to present them to
    GitHub). Use those routes only with a relay you trust. The relay's admin
    API isn't in the open-source build, so per-person relay tokens need signed
    tokens or a restart to change an allowlist.
16. **The direct phone path is plain HTTP.** Use the relay tunnel or a
    tailnet, or a LAN you trust.
17. **One `serve.token` for every direct-path client.** Revoking one direct
    phone means rotating it for all. Tunnel phones are revoked one by one.
18. **Desktop runtimes run as you.** Claude Code and other CLI agents run
    with your OS permissions within their own permission mode; isolation
    is a git worktree, not a sandbox. Kubernetes executors are the
    sandboxed path, and their controller isn't built yet.
19. **Revoking a device doesn't rotate the key.** A revoked device's new
    events are refused, but it still holds every key it had. Rotation happens
    only through **Remove & revoke** on a member (and a few invite paths); for
    your own lost device, move to a new workspace. See
    [Runbooks](runbooks.md#compromised-device).
20. **Outside dearth mode, the default identity mode accepts events from a
    device with no certificate.** During dearth mode, events no certified
    device vouches for, and devices awaiting their second approval, are held
    in every identity mode. See
    [Dearth mode](../concepts/security.md#dearth-mode-a-tighter-entrance-on-demand).
21. **Pre-encryption history isn't shown.** A device with a workspace key
    never applies a plaintext body from the relay, so events a workspace
    stored on a relay before it had a key don't appear on devices that sync
    them later. Devices that already held them keep them.

## Out of scope {#out-of-scope}

- A compromised OS account or machine that runs Ligus.
- Model providers' and MCP servers' handling of what turns send them.
- Availability against a determined denial of service on your relay.

## Reviewer findings {#reviewer-findings}

Security review findings that add to or change this threat model are listed
here with their status. The review of November 2026 covered the core, the
host (desktop, CLI, `ligus serve`), the Helm chart and relay v0.2.0.

### Fixed in 2.0.0 {#findings-fixed}

| Area | Finding | Now |
|---|---|---|
| Core | Any member (or an unsigned event) could revoke someone else's device or remove a member from the trust roster, and every peer then quarantined the victim | Revoking a device and removing a member need Admin; both the ingest check and the trust roster ignore anyone below it |
| Core | A removed member could self-enroll back under an old key and certify a fresh device | Self-enrollment is refused for a removed account until an Admin re-adds it |
| Core | Dearth mode's second device approval bound nothing in the default (Relay) identity mode, and a device that never published a certificate walked around it | During dearth mode, devices awaiting approval and events no certified device vouches for are held in every identity mode |
| Core | The brood gate matched the proposal's declared file list, not its diff, and case-sensitively | The human gate reads the paths in the diff itself, case-insensitively |
| Core | A human approval survived a re-upsert that changed the patch | An approval is void once the diff, file list, kind or workflow changes |
| Core | A lease beat stamped in the future stayed fresh forever; a claim could jump to the maximum epoch and freeze a scope | Beats more than 2 minutes ahead are ignored; epoch jumps are bounded |
| Core | Key seeds were written with the default umask (often `0644`) | Seeds are `0600` in a `0700` key dir |
| Host | Ligus's own git calls ran repository hooks and fsmonitor | They never do |
| Host | Agent CLIs, MCP servers, pre-flight and git inherited Ligus's secrets | Ligus secrets are scrubbed from child processes; provider keys still pass |
| Host | Restricted egress still loaded the repository's Claude Code settings and hooks | Under restricted egress Claude Code loads user settings only, with hooks off |
| Host | Chat markdown loaded remote images, an exfiltration channel | Remote images render as links |
| Host | A keyed workspace accepted plaintext bodies from the relay, so a relay could inject events | Keyed workspaces skip any unsealed body |
| Host | `ligus enroll` wrote `config.toml` world-readable | `0600`, data dir `0700` |
| Host | The worker's tool role gate didn't hold subprocess CLI agents | Below `LIGUS_MCP_MIN_ROLE`, Claude Code, Antigravity and Copilot run read-only and the others are refused |
| Chart | DNS egress allowed port 53 to any address | DNS only to `networkPolicy.dns` (kube-dns by default) |
| Chart | Any pod in the namespace, MCP servers included, reached `serve:8787` | Only this release's own pods by default |
| Chart | Executors' first seconds could precede their NetworkPolicy | A gate holds them until the policy demonstrably blocks traffic |
| Chart | MCP server images by tag | Digest required unless `allowUnpinned` |
| Relay | A malformed `LIGUS_RELAY_TOKEN_PUBKEY` fell back to open | The relay refuses to start |
| Relay | Client-supplied `Fly-Client-IP` was trusted everywhere | Only on Fly; elsewhere `LIGUS_RELAY_TRUSTED_PROXIES` decides |
| Relay | Unbounded ids, list responses and memory-store growth | Id limits (`400`), `LIGUS_RELAY_MAX_LIST_BYTES`, `LIGUS_RELAY_MAX_STORE_BYTES` (`507`) |
| Relay | The Helm chart's tunnel defaulted to open | Off, like the binary |

### Open {#findings-open}

These are in [Known limitations](#known-limitations) and stay there until a
release fixes them: headless devices stuck at key epoch 0, unsigned key
rotations, self-declared vote roles, retroactive hoisted checks, agents that
can read the data dir, no agent identity, and votes through an owner-enrolled
serve box counting as the owner. On Kubernetes, DNS tunnelling through the
cluster resolver remains possible (see
[Kubernetes](../deploy/kubernetes.md#residual-risks)).

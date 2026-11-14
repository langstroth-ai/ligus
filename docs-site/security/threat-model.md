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
exchanged out of band, and rotations are signed events on the config log,
sealed only to keys devices published there. It can't inject a key: keyring
rows on the relay are used only to read, never to seal. It can't pin a
device's sync with a body under an unknown key either (such bodies are
parked). It can't inject plaintext: a device that holds a workspace key skips
any body from the relay that isn't sealed. In the tunnel it can't read or
alter phone traffic (Noise, with the box's key pinned from the QR code). A
ligus-auth token goes only to the relay the session was signed in for. One
gap remains (see [Known limitations](#known-limitations)): the GitHub-backed
routes see the GitHub tokens clients send them.

Each invite carries a fingerprint of the founder's account key, and a device
pins it when it joins, so a hostile relay can't substitute the founder's key.
Every device verifies and folds by the same rules whatever its environment:
`LIGUS_IDENTITY_MODE` is ignored since 2.0 (a set value logs a warning).

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
join in dearth mode, vote as someone else or above their role, forge the
founder, or revoke another person's device (that needs Admin, and every
device ignores a revocation or removal from anyone below it). Each event is
judged by the roster in force when it was made, so a later promotion doesn't
validate an earlier step and a later demotion doesn't undo one. After removal
they can't come back: joining needs a single-use invite an Admin issued, and
a removed account also needs an Admin to re-add it; a removed member's new
events are refused on every device. After removal with key rotation they can't read new
traffic on any host, and old-key writes from them are refused (see the
attachment caveat in [Known limitations](#known-limitations)).

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

Stated plainly, as of 2.0.0. Items marked **(Phase 0, Track B)** or
**(Phase 0, Track D)** are planned for the rest of the Phase 0 refactor;
each stays on this list until a release fixes it.

1. **Secrets in the OS secret store, with a file fallback.** Account,
   device and key-agreement seeds, the workspace passphrase and epoch keys,
   relay and provider tokens and the ligus-auth session live in the OS secret
   store: on macOS a dedicated keychain file inside the data dir (its unlock
   password is a login-keychain item that trusts only Ligus), the Secret
   Service on a Linux desktop, `systemd-creds` on a headless Linux box.
   `settings.json` and `config.toml` hold only `{"set": true}`
   placeholders. Where no store works (Windows, a locked or missing
   keychain, a box without `systemd-creds`) Ligus falls back to `0600` files
   and says so in **Settings → Passkeys & keys**. Anything running unconfined
   as your OS user (not an agent, which the sandbox keeps out of the data
   dir) can still ask the Secret Service or, on macOS, prompt for the unlock
   item.
2. **An invite carries keys.** A `hivews1:` code carries the passphrase and
   every authorized epoch key the inviter holds, so the joiner can read the
   history. Treat it like a password and send it over a trusted channel.
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
4. **Agent output signed by its host.** Hosts certify a key for each agent
   bound to them, and that agent's replies and Ligus tool effects are signed
   with it, judged by the capped Agent tier (never a vote, an apply or an
   admin change) and retired when the agent moves to another host. A host
   that can't certify a key (not yet certified, or the device-local Home)
   signs for the agent itself; under rules version 2 that output, and every
   effect of the chat's primary runtime (stamped `runtime_effect`), is judged
   at the Agent tier too. Under rules v1 it is judged by its host's role.
5. **Attachments and host presence beats use the passphrase key.** Chat
   events follow signed key rotations on every host, but attachment blobs and
   the hosts' liveness beats are still sealed under the passphrase-derived
   key (epoch 0), which a removed member who knew the passphrase keeps.
   After offboarding someone, treat attachments sent later as readable by
   them, or [move to a new workspace](runbooks.md#move-to-a-new-workspace).
6. **Signed rotations rely on what a device folds.** A rotation counts only
   when an Admin's certified device signed it at the next version on the
   config log, and a device seals only under such a rotation; junk or
   unsigned keyring rows on the relay are never used. A device that has no
   founder pin for a workspace (joined without a pinned invite, such as a DM's
   second side) adopts no rotation and keeps sealing under the passphrase
   key. A device that isn't a recipient of the newest rotation doesn't write
   until the next rotation includes it.
7. **Presence needs rules v2, and a passkey on every workspace.** Under
   rules version 2 membership and role changes, device revocation, key
   rotation, passkey removal, dearth mode, lockdown clears, path zones,
   workflow definitions, agent host or runtime changes, lowered gates and
   human-gated votes need a passkey assertion over the event inside its
   signed stamp, verified on every device. Under rules v1 (the default until
   an Owner turns v2 on) they need signature and role only. Passkeys are
   registered per workspace (on its config log). The desktop runs WebAuthn
   in your browser on `https://ligus.ai/presence`; a compromised page there
   can't forge a device signature, but it could ask your authenticator to
   sign a different event during a gesture you started. The desktop checks
   the returned challenge is its own, and the sheet shows the digest so you
   can compare it with the page.
8. **Phone approvals through a box carry your passkey.** A `ligus serve`
   box enrolled as your device signs as you, so votes, lockdown clears and
   agent rebinds from the phone go through `presence/prepare` and
   `presence/commit`: the phone's passkey (Face ID) signs the event's digest
   and the vote counts as the passkey's account. Without it the box answers
   `403 presence_required` and every device's fold drops such a vote.
9. **Some agents still ask for actions in reply text.** Agents with tool use
   get the typed [Ligus tools](../addons/ligus-mcp.md), and the subscription
   CLIs run over [ACP](../addons/acp.md). aider, pi, Hermes, and Codex and
   Antigravity on their headless adapters still use the `[[…]]` directives,
   which every host (desktop, worker, serve) now parses the same way. A
   directive can file only what the matching tool can.
12. **Agents can read every chat in the workspace.** `ligus_read_chat` reads
    any chat of the workspace the agent runs in (by design), so a prompt
    injection planted in one chat can reach agents working in another. Read
    results are marked as data, every read is in the device-local audit with
    the chat it read, and reads are budgeted (500 messages per turn). A
    locked-down or allow-listed chat can't be read from a chat with wider
    egress.
13. **A paused turn holds its host.** A turn waiting on `ligus_ask_human` or
    an ACP permission request keeps its process and chat until someone
    answers or its timeout passes (at most an hour; 10 minutes for permission
    requests). Stop the turn to release it. A restart cancels it; questions
    can't be resumed in 2.0.
10. **Sign counters aren't checked.** Synced passkeys report a signature
    counter of 0, so Ligus doesn't use it to detect cloned authenticators.
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
    tokens (short-lived ones from [ligus-auth](../deploy/ligus-auth.md), for
    example) or a restart to change an allowlist.
16. **The direct phone path is plain HTTP.** Use the relay tunnel or a
    tailnet, or a LAN you trust.
17. **One `serve.token` for every direct-path client.** Revoking one direct
    phone means rotating it for all. Tunnel phones are revoked one by one.
18. **Revoking a device doesn't rotate the key.** A revoked device's new
    events are refused, but it still holds every key it had until an Admin
    rotates ([Rotate the workspace key](runbooks.md#rotate-workspace-key)).
    A removed member who writes with an old key is refused once a device
    knows of the removal; an event already accepted before that stays (the
    choice of what was accepted depends on arrival, like revocation).
19. **Pre-encryption history isn't shown.** A device with a workspace key
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
| Host | Agent CLIs, MCP servers, pre-flight and git inherited Ligus's secrets | Child processes start from an allow-listed environment: no Ligus secrets, and only the provider key configured for that runtime |
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
| Core | Proposal-set vote roles were self-declared, so a Viewer's vote claiming Admin met an Admin floor | A vote counts only from the voter it names, at their role on the roster when they cast it, for the content digest it names; status is derived from counted votes |
| Core | Hoisted checks were retroactive: a later demotion undid decisions, a later promotion validated earlier steps | Every event is judged at its own position on the workspace config log; events whose position hasn't synced wait instead of being dropped |
| Core | Key possession alone let a non-member or ex-member post and self-enroll | Every author must be a member at the event's position; joining needs an Admin's single-use invite |
| Core | Signatures covered re-encoded JSON, so fields an older build didn't know were lost or broke verification | Envelopes are verified, stored and re-sent as the bytes received |
| Core | A removal in one workspace revoked devices in another; the identity mode came from the environment | Trust rosters are per workspace with the founder pinned from the invite; folds never read the environment |
| Host | Agents could read the data dir (keys, settings, `serve.token`) and reach the network directly | Agent processes run in the OS sandbox with a per-turn egress proxy and an allow-listed environment |
| Host | The tool role gate only held on headless hosts and read today's roster | One author gate on every host, at the author's role when the message was posted |
| Host | Repository-supplied runtimes and MCP servers ran on clone | Inert until enabled on this device by digest |
| Host | Votes through an owner-enrolled serve box counted as the owner's | They count only with the owner's passkey over the vote (`presence/prepare`, `presence/commit`); without it the box answers `403 presence_required` and every fold drops them |
| Keys (Track B) | Key rotations were unsigned; a desktop sealed under any key it could open from the relay, so a removed member or an outsider who knew device keys could inject one | Rotations are signed `WorkspaceKeyRotated` events (Admin, next version, key commitment) sealed only to keys devices published on the config log; devices seal only under the newest authorized rotation |
| Keys (Track B) | `ligus serve`, `ligus worker` and the CLI stayed on epoch 0 | One keyring for every host: they read and write rotated epochs |
| Keys (Track B) | An event sealed under an old key was accepted from someone who had already folded the rotation, or from a removed member | Refused by every fold (sealed epoch below the one at its author's frontier) and, for non-members, at ingest |
| Keys (Track B, A-13) | A body sealed under an epoch nobody holds pinned every device's sync cursor | Parked on the device and retried when the keyring changes |
| Core (A-14) | A frontier naming a config event nobody will deliver held its proposal's side effects forever | Holds end once the config log passes the claimed position (or it is far ahead) |
| Secrets (Track B) | Seeds and `settings.json` secrets were plain files readable by anything running as the user | OS secret store (a dedicated macOS keychain in the data dir, Secret Service, `systemd-creds`); files keep `{"set": true}` placeholders |
| Secrets (C-11) | A subscription CLI agent keeps login-keychain access and could ask for items that trust `/usr/bin/security` | Ligus's items are in their own keychain file under the data dir, which every agent sandbox denies |
| Presence (Track B) | No human presence on admin events | Rules v2: passkey assertions over the event digest inside the signed stamp, verified offline on every device |
| Relay (E-5) | The ligus-auth access token was sent to any configured relay, an invite's included | Only to the relay origin its session was signed in for |
| Host (C-9) | An agent could write hooks or MCP servers into `~/.claude`, `~/.codex` and the other CLIs' config, which ran unsandboxed on the user's next CLI start | CLI agents run with a per-turn home seeded with the top-level config files; their real config dirs aren't writable, and only refreshed sign-in files are copied back ([details](../addons/acp.md#cli-home)) |
| Host (C-13) | A macOS agent could drive other apps with Apple Events or read the pasteboard | The Seatbelt profile denies Apple Events and the pasteboard services |
| Host (C-14) | A runtime's trust digest didn't cover its provider id or metrics endpoint | It covers both, and a non-default driver |
| Agents | Agents filed actions through reply text, and every host but the desktop ignored it | Typed Ligus tools with a per-turn credential, budgets, an audit trail and agent-signed effects; one directive parser on every host |
| Agents (Track D review) | ACP `fs/write_text_file` followed a dangling symlink out of the worktree, matched protected paths by exact case and only at the top level, and `fs/read_text_file` loaded any file (or blocked on a FIFO) in the app's process | Symlinks are refused and writes open with `O_NOFOLLOW`; protected names match at any depth ignoring case; reads are capped at 16 MiB and only regular files are served |
| Agents (Track D review) | An ACP permission request was granted by the first answer from any member, on any device | Only the host's own person or the agent's owner can grant it |
| Agents (Track D review) | The CLI credential copy-back read through symlinks and hard links the agent left in its turn home | Read without following links, regular single-link files only |
| Agents (Track D review) | `ligus_read_chat` could read a locked-down chat from a chat with open egress | Another chat is readable only when the reader's egress is at least as strict |
| Agents (Track D review) | The config-impact badge missed case variants, paths in shell strings, `/usr/bin/git config` and inline code; a primary runtime's proposals were badged as a person's | Case-insensitive, shell-tokenized matching; inline interpreter code is flagged; only roster members badge as human |
| Agents (Track D review) | `agentQuorum` could lower a set's scout quorum; uncharged calls flooded the audit ring; one endless socket line grew the app's memory | Raise-only quorum; every call counts toward `maxToolCalls`; socket lines are capped at 4 MiB |
| Agents (Track C/D residuals) | An ACP adapter or legacy CLI adapter could write an unbounded stdout line (or endless output, or stderr) and grow the app's memory | Agent stdout lines are capped at 8 MiB and a turn's output at 256 MiB (`LIGUS_AGENT_MAX_LINE_BYTES`, `LIGUS_AGENT_MAX_OUTPUT_BYTES`); past either the turn ends with an error and the agent is stopped. 256 KiB of stderr is kept for error messages |
| Agents (Track C/D residuals) | ACP `fs/*`, the built-in file tools and the CLI-home copy-back checked a path and then opened it by name, so a directory swapped for a symlink in between redirected the host's read or write outside the worktree | Files are opened beneath the turn's directory one component at a time with `openat` (`O_NOFOLLOW` on every component, `O_DIRECTORY` for parents); a symlink anywhere on the way fails the open (macOS and Linux; Windows keeps the path checks only) |
| Agents (Track C/D residuals) | An agent could write a valid sign-in for a different account into its turn home, and the copy-back installed it as the user's CLI sign-in | Copy-back requires the same keys and the same account fields (account id, email, user, API key, `id_token` claims) as the file it replaces; otherwise it is dropped and logged. Claude Code's file has no account field, so only its plan and shape are checked |
| Release | `scripts/release-matrix.sh` passed API keys as curl arguments, visible in `ps` | Headers go through a private file |

### Open {#findings-open}

These are in [Known limitations](#known-limitations) and stay there until a
release fixes them: attachments and host liveness beats on the passphrase key,
rules v1 as the default (presence only once an Owner turns rules v2 on),
devices without a founder pin keeping the passphrase key, and ingest-time
decisions that depend on arrival order. On Kubernetes, DNS tunnelling through
the cluster resolver remains possible (see
[Kubernetes](../deploy/kubernetes.md#residual-risks)).

# Security model

This section is the reference for what Ligus protects, against whom, and
where its protection stops. It has three pages:

- **Security model** (this page): the components, what each holds, the trust
  boundaries in each deployment shape, encryption, roles and the 0.1 guards.
- [Threat model](threat-model.md): attackers, what they can and can't do, and
  the known limitations stated plainly.
- [Runbooks](runbooks.md): device revocation, key rotation, a lost phone,
  offboarding someone, a compromised box.

[Security & trust](../../ligus/concepts/security.md) is the short list of guarantees;
these pages explain them and their edges. Report vulnerabilities as
described in
[`SECURITY.md`](https://github.com/langstroth-ai/ligus/blob/main/SECURITY.md).

## Components and what they hold {#components}

| Component | Secrets it holds | Data it holds | Talks to |
|---|---|---|---|
| **Desktop app** | in the OS secret store: account and device key seeds, an X25519 key-agreement seed for receiving rotated keys, every workspace passphrase and delivered epoch key, API keys and MCP tokens you paste, forge tokens of a code host this device connects (read and write kept apart; never synced, never sent to a relay), the contact write key, relay access tokens, the relay issuer key if you set one | `ligus.db`: every event of every workspace it belongs to, in clear | relay, peers (P2P), model providers, MCP servers, GitHub |
| **`ligus serve`** | in the OS secret store: its device and key-agreement keys, the workspace passphrase and epoch keys, the relay token; on disk: `serve.token`, tunnel host keys, per-phone push keys, `LIGUS_WS_SECRET_*` and MCP tokens from its environment, the host's `gh` login | `ligus.db`, checkouts under `LIGUS_REPOS_DIR` | relay, phones, model providers, MCP servers, GitHub |
| **`ligus worker`** | in the OS secret store: its device and key-agreement keys, the workspace passphrase and epoch keys; `LIGUS_WS_SECRET_*` and MCP tokens | `ligus.db`, bound checkouts | relay, model providers, MCP servers |
| **Relay** | token-issuer **public** key, hashed access tokens, APNs key (if push), DB password | ciphertext envelopes, keyring blobs, metadata (see [Self-hosting a relay](../networking/self-host.md#what-the-relay-holds)) | every device, Apple (APNs), GitHub API |
| **Phone app** | the direct-path bearer token and/or its tunnel credential, Noise key and push key, per host profile, in the iOS Keychain | what it has fetched and cached | `ligus serve` (directly or through the relay) |
| **Model providers, MCP servers** | | whatever a turn sends them: prompts, chat history, and file contents when file tools or a CLI agent read them | |

The desktop and the CLI keep their secrets in the OS secret store (Phase 0,
Track B): on macOS a dedicated keychain file inside the data dir, unlocked by
a login-keychain item only Ligus can read (on macOS there is no silent
fallback to files: `LIGUS_SECRET_BACKEND=file` opts in); the Secret Service
on a Linux desktop; `systemd-creds` on a headless Linux box; mounted Secrets
on Kubernetes; owner-only `0600` files as a fallback elsewhere, with a
warning.
`settings.json` and `config.toml` keep `{"set": true}` placeholders. Agents
can't reach any of it: the data dir, keychain file included, is denied to
every agent sandbox, even to subscription CLIs that keep login-keychain
access. `LIGUS_SECRET_BACKEND` (`keychain`, `secret-service`,
`systemd-creds`, `k8s`, `file`) picks the backend on a device. See
[known limitations](threat-model.md#known-limitations).

## Encryption {#encryption}

- **Workspace events** are signed by the authoring device (Ed25519) and, for
  relay sync, sealed with ChaCha20-Poly1305 under the workspace key before
  they leave the device. Sync refuses to push without a key, and a device
  that holds a key never applies an unsealed body from the relay.
- **The workspace key** starts as a passphrase in the `hivews1:` invite
  (epoch 0). A rotation (epoch 1, 2, …) is a signed `WorkspaceKeyRotated`
  event on the workspace config log: an Admin's certified device issues it at
  exactly the next version, with a commitment to the new key
  (`ligus-workspace-key-commit-v1`), sealed to the X25519 key-agreement keys
  each member's devices published themselves on the config log (never a
  relay directory entry). Every host (desktop, `ligus serve`, `ligus worker`,
  the CLI) builds the same keyring: it seals new events only under the
  newest **authorized** rotation, stamping the key id on each body, and
  reads with every key it can open. Old epochs stay readable. An event
  sealed under an older epoch than its author had folded is refused, and so
  is an old-key write from someone no longer a member. Invites carry every
  authorized epoch key, so newcomers read the history.
- **Phone to box**: direct path is plain HTTP with a bearer token, so keep it
  on loopback, LAN or a VPN such as Tailscale (which encrypts). Tunnel path
  is a Noise IK session (`Noise_IK_25519_ChaChaPoly_SHA256`) end to end
  between phone and box; the phone pins the box's key from the pairing QR.
  Secrets never take the plain-HTTP direct path: code-host tokens, device
  sign-in codes and invite codes (which carry the workspace passphrase and
  epoch keys) move only through the tunnel or on a loopback bind; elsewhere
  the box refuses with 403.
- **Presence** (who is typing in which chat, and when someone was last
  active) rides the relay's ephemeral presence slots, sealed under the same
  keyring as events (ChaCha20-Poly1305 at the authorized head, no separate
  key or domain string), as host liveness and coordinator lease beats already
  were. The relay sees a slot name (`human:` and the writing device's id),
  the blob's size and when it changes; not the chat, the person or their
  name. Readers ignore plaintext blobs, beats under an older epoch, beats in
  another device's slot and beats older than 8 s. Beats aren't signed, so a
  current member could fake another member's typing indicator (cosmetic: no
  authority rides on it; names come from the reader's own roster where it
  knows the person). No message text is ever in a beat, and typing never
  triggers a push. Each person can turn sharing off (**Share typing
  status**).
- **Push** content is sealed (ChaCha20-Poly1305) with a per-phone key agreed
  at pairing; Apple and the relay see a generic alert and ciphertext.
- **Peer-to-peer** sync between contacts' desktops runs over iroh (QUIC/TLS).
- **Sealing a key for a device** (X25519, HKDF-SHA256, ChaCha20-Poly1305)
  refuses a low-order X25519 key on either side, since the all-zero shared
  secret it forces is known to anyone. The tunnel's Noise handshake makes the
  same check.
- **TLS** (transport only; content never depends on it): every HTTPS client
  and the relay tunnel's WebSockets check certificates against the OS trust
  store, so a root your administrator installed (for example a
  TLS-inspecting proxy's) works for sync and remote access alike. A host with
  no CA certificates, such as a minimal container, uses Ligus's bundled
  Mozilla roots for both instead.
- **Proxies**: Ligus uses `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and
  `NO_PROXY`, and where those set nothing, the proxy in the macOS or Windows
  network settings. A local proxy works the same way from either source.
  Through a proxy, the proxy resolves the target, so the connect-time
  public-address check doesn't see it. For servers someone else named, Ligus
  resolves the name itself first: a code host must be marked internal to
  reach a non-public address, and a DM relay from an invite whose name
  resolves on this machine to a loopback, private or link-local address is
  refused before each use (when you accept the invite, and before each sync
  or contact call). A name only the proxy can resolve is left to the proxy,
  and a proxy that resolves names differently from this machine can still
  reach what it resolves: keep the proxy from reaching internal addresses
  if invites from strangers matter to you.

## Trust boundaries by deployment shape {#trust-boundaries}

### Solo laptop {#tb-solo}

One boundary: your OS account. Anyone who can read the app data dir can act
as your device. Model and MCP providers see what turns send them.

### Personal fleet {#tb-personal-fleet}

- Laptop and home box are both fully trusted: each holds the workspace key.
  The box holds a device key certified by your account, not your account key.
- The relay is untrusted for content; it sees metadata.
- The phone is trusted **to act through the box**: anyone holding the phone's
  credential (or `serve.token`) can read and write everything the box can,
  at the box's role. With the phone itself (its Secure Enclave key), they
  can also post, vote, answer and ask for an apply as you; a lockdown clear
  and gated votes need your passkey too. It never holds the workspace key.
- The direct path trusts the network between phone and box (Tailscale or
  your LAN).

### Team on a relay {#tb-team}

- Each member's devices hold the workspace key; every member can read the
  whole workspace. **Roles limit what members can change, not what they can
  read.**
- Roles are enforced by every device on every event (authorization on
  ingest), not by the relay. A modified client can author anything, but its
  events are dropped by everyone else when its role doesn't allow them.
- Shared workers hold the workspace key and the runtime secrets given to
  them; whoever administers a worker box can read the workspace.
- The relay operator sees metadata and can deny service, but can't read or
  forge events.

### Kubernetes (preview) {#tb-kubernetes}

- The relay namespace holds no workspace keys.
- Each workspace namespace holds that workspace's key on the serve and
  worker volumes: cluster admins, anyone who can `exec` into those pods or
  read their volumes or Secrets can read the workspace.
- Only the release's own pods reach the serve API by default; MCP server
  pods and other pods in the namespace don't. DNS egress goes to the cluster
  resolver only.
- Executors are designed to hold no workspace or device key; the controller
  that would launch them isn't built.

### Keys off the cluster (design) {#tb-keys-off-cluster}

The cluster would hold no workspace key at all; executors would get
turn-scoped inputs. See [Keys off the cluster](../deploy/keys-off-cluster.md).

## Identity, devices and roles {#roles}

- An **account** (one per person: a key and a random id made on the first
  device, never derived from a provider id; more devices join through an
  enrollment code) certifies **device** keys. A `ligus serve` or `ligus
  worker` box is never one of your devices: it is its own **service member**
  with its own account, admitted once by an Admin with a passkey (see
  [Admitting a box](../ops/admitting-a-box.md)).
- Membership is workspace-wide, kept on the workspace config log. Roles:

| Role | Can |
|---|---|
| **Owner** | everything an Admin can; the last Owner can't be removed or demoted |
| **Admin** | add, remove and re-role members; manage workspace runtimes (they carry credentials); revoke devices (a revocation or removal from anyone below Admin is ignored on every device); dearth mode and join approvals; path zones and the home beacon |
| **Contributor** | messages, proposals and votes, proposal sets, agents, skills, vaults, workflows, channels, hosts, leases; clear a chat lockdown (humans only) |
| **Viewer** | read; raise a chat lockdown |

A **service member** (a serve or worker box) holds at most Contributor. Its
votes count at its role but never as a person's approval, it carries no
passkey or presence, and it removes, demotes, admits and revokes no one (its
own device excepted). A person's invite never admits a box, and a box
admission never admits a person.

Every event is judged by one authorization engine, the same on every device,
against the roster **in force when it was made**: its author must have been a
member then, with the role the event needs. A non-member's events are
refused, joining needs a single-use invite an Admin issued, and a later
promotion or demotion never changes an earlier verdict. Event kinds with no
explicit rule require Owner. Proposal sets add their own decision role on
top, and a vote counts at the voter's role when cast, for the exact content
it names.

- **Author gate on every host.** On the desktop, `ligus worker`, `ligus
  serve` and `ligus agent`, a turn runs at its configured tier only if the
  message's author held the host's floor when they posted it: **Admin** when the turn can write,
  **Contributor** when it is read-only (`LIGUS_MCP_MIN_ROLE` overrides it; a
  lower value logs a warning). Below it, Ligus's own tool loop runs without
  tools, Claude Code, Antigravity and Copilot run read-only, and Codex,
  aider, pi and Hermes are refused with a note. An author that can't be
  resolved gets no tools. A turn an agent triggers (a hand-off, an @mention
  cascade, a workflow stage) runs at the lowest role of the humans who
  started the chain, never at the tier of the host that signed the agent's
  reply. See [the author gate](../../ligus/concepts/security.md#author-gate).
- **Worker write access** is off by default:
  `LIGUS_WORKER_PERMISSION_MODE` must raise it, and then only Admins' messages
  drive write-capable turns there unless you lower `LIGUS_MCP_MIN_ROLE`.
- **Agent processes are sandboxed.** No agent process can read Ligus's keys,
  settings, database or `serve.token`, write outside its worktree, or reach
  the network except through a per-chat allow-list. See [the OS
  sandbox](../../ligus/concepts/security.md#os-sandbox).
- **Phone and serve approvals.** A `ligus serve` box signs only as itself,
  a service member whose votes never count as a person's. What only a person
  may do from the phone is signed by the **phone's own device key** (with your
  passkey where the rules need it), and the box drafts and relays it (it can
  hold it back, never alter or forge what the phone checked before signing):
  box admissions (`POST v1/admissions/…`) and the
  [phone actions](../reference/serve-api.md#phone-actions) (`POST
  v1/phone-actions/…`: post, vote, answer, clear a lockdown, Apply on
  ‹host›, withdraw). A vote counts as a person's only from a device certified
  for that person's account. Nothing applies a proposal on approval: one
  host a person names applies it, once.
- **Agents** sign with their own host-certified keys, judged by a capped
  Agent tier that never votes, applies or administers. They act through the
  [Ligus tools](../../ligus/addons/ligus-mcp.md): proposals only, behind a per-turn
  credential, with budgets and an audit trail. A host that can't certify a
  key signs for the agent (see
  [Known limitations](threat-model.md#known-limitations)).

## Passkey presence {#presence}

Signatures prove which device made an event, not that its person was there.
Every workspace is on **rules v2** from its first event: creating one asks
for your passkey and a recovery key, and the events that change who can do
what need a **passkey** assertion over the event:

| Category | Events |
|---|---|
| Membership and roles | adding someone else, removing, changing a role, join approvals |
| Devices | revoking a device (your own included) |
| Keys | key rotation, removing a passkey, account key rotation |
| Guards | dearth mode, clearing a lockdown |
| Zones | path zones and the home beacon |
| Workflows | workflow definitions; votes on a proposal that carries a workflow |
| Agents and runtimes | an agent's host or runtime changing, opening an agent to everyone ([agent access](../../ligus/features/agent-access.md)), removing an agent, workspace runtimes, someone else's host |
| Gates | tuning a proposal set, lowering a proposal's approvals or role floor, the workspace rules (version and relying party; not the one written at creation) |
| Human-gated decisions | votes on a human-gated proposal (lockdown, dearth over brood paths), a person deciding a proposal set, applying a human-gated proposal |

- **What is signed.** The WebAuthn challenge is SHA-256 of
  `ligus-presence-v1\0`, the workspace, chat and event ids, the event's
  position, its exact payload bytes, its config frontier and its author's
  account. The assertion rides inside the actor stamp, which the device
  signature covers, so it can't be stripped, moved or replayed onto another
  event.
- **Verified everywhere, offline.** Every device checks it while folding:
  the workspace's relying party (`ligus.ai` and origin `https://ligus.ai`
  unless a self-hosted deployment named its own when the workspace was
  created), user presence **and** user verification, a passkey registered to
  the event's author on the
  config log at that position, ES256 or EdDSA. Signature counters aren't
  used (synced passkeys report 0).
- **Any passkey.** Touch ID and iCloud Keychain, Windows Hello, Google
  Password Manager, phones over QR, YubiKeys. Secure Enclave-only keys aren't
  required. The desktop runs the ceremony in the system passkey dialog on
  macOS (for a relying party the app is associated with) and Windows, and
  otherwise in your browser on the relying party's `/presence` page; either
  way it checks the result is for its own challenge
  ([Native passkeys](native-passkeys.md)). The iPhone app uses
  `ASAuthorization` directly.
- **Registration.** A passkey is registered per workspace on its config log.
  The account's first proves itself (its own assertion over the
  registration); every later one needs an existing passkey. Keep at least
  two, plus the **recovery key**: 24 words, an Ed25519 key accepted wherever
  a passkey is (`ligus-recovery-v1`). See [Lost a
  passkey](runbooks.md#lost-passkey).
- **Agents never satisfy presence.** Host-signed agent output
  and every effect of a chat's primary runtime are judged at the Agent tier,
  which can't vote or administer.

## The 0.1 guards {#guards}

| Guard | What it does | Who can switch it | Details |
|---|---|---|---|
| **Sandbox envelope** | per turn: isolated worktree, protected dirs (`.git`, `.ligus`, `.claude`, `.vscode`, config files), egress policy, secret redaction in logged replies; only ever tightens | automatic | [The sandbox envelope](../../ligus/concepts/security.md#the-sandbox-envelope-around-every-agent-turn) |
| **OS sandbox** | every agent process: Seatbelt (macOS) or bubblewrap + Landlock + seccomp (Linux; Landlock + seccomp without namespaces where bubblewrap can't run, see [tiers](../ops/linux-sandbox.md)); no reads of Ligus's data, keys or `serve.token`; writes only in the worktree; network only through the turn's egress proxy (plus opt-in [egress presets](../../ligus/concepts/security.md#egress-presets), off by default); agents can't commit; where no sandbox exists every agent process is refused (any tier) and HTTP models answer without local tools | automatic | [The OS sandbox](../../ligus/concepts/security.md#os-sandbox), [Sandbox defaults](../../ligus/concepts/security.md#sandbox-defaults), [Linux tiers](../ops/linux-sandbox.md) |
| **Home default-deny** | every agent process runs with a per-turn home; your real home is unreadable and unwritable except the worktree, a read-only toolchain allow-list and signed grants | automatic; grants: Admin with a passkey | [Your home dir is default-deny](../../ligus/concepts/security.md#home-default-deny) |
| **Owner-only shell** | a CLI's shell tool, ACP `execute` and `run_command` only for turns the workspace Owner starts (the device owner in the local workspace), or for an agent an Owner granted it | Owner, per agent, with a passkey | [The shell is the Owner's](../../ligus/concepts/security.md#owner-only-shell) |
| **Workspace trust** | repo-supplied runtimes and MCP servers stay inert until enabled on this device, per exact command | you, per device | [Trust this workspace](../../ligus/concepts/security.md#workspace-trust) |
| **Dearth mode** | two Admins for new members; cross-account approval for new devices; agents limited to allow-listed MCP servers; human approval for protected (brood) paths | Admin | [Dearth mode](../../ligus/features/guards.md#dearth-mode) |
| **Chat lockdown** | on an injection marker, a credential in a diff or an unrequested CI change: no network or MCP for that chat's agents, human approval on every proposal | raised by anyone (or automatically); cleared by a human Contributor+ | [Chat lockdown](../../ligus/features/guards.md#chat-lockdown) |
| **Leases** | exactly one device drives a workflow run or the workspace coordinator role; signed heartbeats over relay presence; epoch fencing | Contributor (signed by the holder device) | [Which device drives a run](../../ligus/features/workflows.md#which-device-drives-a-run) |
| **Ligus tools** | agents act through typed tools bound to one turn (an HMAC credential that expires with it): proposals only, never votes, applies or admin changes; budgets per turn; a device-local audit per call and a synced summary per turn | automatic; budgets per device (`LIGUS_MCP_MAX_*`) | [Ligus tools for agents](../../ligus/addons/ligus-mcp.md) |
| **Exfiltration guard** | once a turn has read project files, MCP calls are refused | on by default; CLI can disable with `LIGUS_MCP_AFTER_READS` | [MCP servers](../../ligus/addons/mcp.md) |
| **Hygiene sweep** | removes leftover worktrees (saving patches), marks un-appliable proposals stale | automatic | [Hygiene](../../ligus/features/guards.md#hygiene) |

All guard switches are events on the workspace config log and are
re-checked on ingest by every device.

## Security review notes {#review-notes}

Findings from security reviews that change what this section says are
recorded here, each with the date and the page section it affects. The full
list with status is in
[Reviewer findings](threat-model.md#reviewer-findings).

**2026-11-14: Phase 0 Track D (agent interface).**

- [Roles](#roles): agents bound to a host sign their replies and tool
  effects with their own certified keys; the Ligus MCP server checks the
  Agent tier before signing.
- [The 0.1 guards](#guards): the OS sandbox allows exactly one extra path, the
  turn's Ligus MCP socket. CLI agents run with a per-turn home, so they can't
  persist hooks or MCP servers into your real CLI config (C-9). On macOS the
  profile also denies Apple Events and the pasteboard (C-13).
- Implement on a command proposal runs its stored argv exactly, sandboxed and
  offline; the preview shows the same plan the runner uses.

**2026-11-13: core, host, chart and relay v0.2.0 review.**

- [Roles](#roles): revoking a device needs Admin, and both ingest and the
  trust roster ignore anyone below it. A removed member can't enroll
  themselves back; only an Admin's re-add lifts the removal.
- [The 0.1 guards](#guards): dearth mode holds devices awaiting their second
  approval, and events no certified device vouches for, in every identity
  mode. The human gate for protected paths reads the paths in the diff
  itself, case-insensitively, and an approval is void if the patch changes
  afterwards. Lease beats stamped more than 2 minutes in the future are
  ignored.
- [Components](#components): key seeds are `0600` in a `0700` key dir;
  `config.toml` is `0600` and the CLI data dir `0700`.
- [Encryption](#encryption): a keyed workspace rejects plaintext bodies from
  the relay, so plaintext history from before the workspace had a key isn't
  shown on devices that fetch it later.
- Agent turns: Ligus's own git calls never run repository hooks or
  fsmonitor; Ligus's secrets are scrubbed from agent CLIs, MCP servers,
  pre-flight and git, while model provider keys pass through; under
  restricted egress Claude Code ignores the repository's settings and hooks;
  remote markdown images render as links. See
  [Prompt injection](threat-model.md#prompt-injection).
- [Tool gate](#roles): below `LIGUS_MCP_MIN_ROLE`, subprocess CLI agents on
  workers and serve boxes run read-only or are refused; raise it on any
  worker with write access.
- [Kubernetes](#tb-kubernetes): DNS only to the cluster resolver, serve
  reachable only from the release's pods, executors held until their
  NetworkPolicy is in force, MCP images pinned by digest. See
  [Kubernetes](../deploy/kubernetes.md#hardening-the-charts-apply).
- Relay v0.2.0: a malformed `LANGSTROTH_RELAY_TOKEN_PUBKEY` refuses to start;
  per-client limits use `LANGSTROTH_RELAY_TRUSTED_PROXIES`, and `Fly-Client-IP`
  is trusted only on Fly; tokens can be confined to workspaces
  (`LANGSTROTH_RELAY_WORKSPACE_SCOPE`); the chart's tunnel is off by default. See
  [Self-hosting a relay](../networking/self-host.md).

**2026-11-14: Phase 0, Tracks A, C and E.**

- [Roles](#roles): one authorization engine judges every event at its
  position on the workspace config log; votes count at the signer's role
  then, for the content digest they name, and proposal status is derived
  from counted votes. Every author must be a member at the event's position;
  joining needs an Admin's single-use invite. Envelopes are verified and
  re-sent as the bytes received. Trust rosters are per workspace, with the
  founder pinned from the invite, and nothing in a fold reads the
  environment (`LIGUS_IDENTITY_MODE` is ignored).
- [The 0.1 guards](#guards): agent processes run in the OS sandbox with a
  per-turn egress proxy and an allow-listed environment; repository-supplied
  runtimes and MCP servers are inert until trusted on the device; the author
  gate applies on every host at the author's role when they posted. Turn
  measurements are device-local facts.
- [Phone and serve approvals](#roles): a delegated serve box's votes count
  only with the owner's passkey over them.
- [Signed key rotations](#encryption) and one keyring on every host;
  [secrets in the OS secret store](#components); [passkey presence](#presence)
  on every workspace from its first event (rules v2; no rules v1).
- Organization sign-in: [langstroth-auth](../deploy/ligus-auth.md) issues
  short-lived relay tokens from your identity provider.
- Still open: see [Known limitations](threat-model.md#known-limitations).

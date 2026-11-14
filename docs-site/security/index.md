# Security model

This section is the reference for what Ligus protects, against whom, and
where its protection stops. It has three pages:

- **Security model** (this page): the components, what each holds, the trust
  boundaries in each deployment shape, encryption, roles and the 2.0 guards.
- [Threat model](threat-model.md): attackers, what they can and can't do, and
  the known limitations stated plainly.
- [Runbooks](runbooks.md): device revocation, key rotation, a lost phone,
  offboarding someone, a compromised box.

[Security & trust](../concepts/security.md) is the short list of guarantees;
these pages explain them and their edges. Report vulnerabilities as
described in
[`SECURITY.md`](https://github.com/ligus-ai/ligus/blob/main/SECURITY.md).

## Components and what they hold {#components}

| Component | Secrets it holds | Data it holds | Talks to |
|---|---|---|---|
| **Desktop app** | account key seed and device key seed (`keys/`), every workspace key (`settings.json`), an X25519 key-agreement seed for receiving rotated keys, API keys and MCP tokens you paste, GitHub token, relay access tokens, the relay issuer key if you set one | `hive.db`: every event of every workspace it belongs to, in clear | relay, peers (P2P), model providers, MCP servers, GitHub |
| **`ligus serve`** | its device key, the workspace key (`config.toml`), `serve.token`, tunnel host keys, per-phone push keys, `LIGUS_WS_SECRET_*` and MCP tokens from its environment, the host's `gh` login | `hive.db`, checkouts under `LIGUS_REPOS_DIR` | relay, phones, model providers, MCP servers, GitHub |
| **`ligus worker`** | its device key, the workspace key, `LIGUS_WS_SECRET_*` and MCP tokens | `hive.db`, bound checkouts | relay, model providers, MCP servers |
| **Relay** | token-issuer **public** key, hashed access tokens, APNs key (if push), DB password | ciphertext envelopes, keyring blobs, metadata (see [Self-hosting a relay](../networking/self-host.md#what-the-relay-holds)) | every device, Apple (APNs), GitHub API |
| **Phone app** | the direct-path bearer token and/or its tunnel credential, Noise key and push key, per host profile, in the iOS Keychain | what it has fetched and cached | `ligus serve` (directly or through the relay) |
| **Model providers, MCP servers** | | whatever a turn sends them: prompts, chat history, and file contents when file tools or a CLI agent read them | |

The desktop and the CLI store private key seeds as plain files, owner-only
(`0600` in a `0700` directory); see
[Key storage](../concepts/identity.md#key-storage) and
[known limitations](threat-model.md#known-limitations).

## Encryption {#encryption}

- **Workspace events** are signed by the authoring device (Ed25519) and, for
  relay sync, sealed with ChaCha20-Poly1305 under the workspace key before
  they leave the device. Sync refuses to push without a key, and a device
  that holds a key never applies an unsealed body from the relay.
- **The workspace key** starts as a passphrase in the `hivews1:` invite
  (epoch 0). Rotations (epochs 1, 2, …) are sealed to each remaining device's
  X25519 key-agreement key and published through the relay; a device seals
  new events under the highest epoch it holds and opens each event with that
  event's own epoch.
- **Phone to box**: direct path is plain HTTP with a bearer token, so keep it
  on loopback, LAN or a VPN such as Tailscale (which encrypts). Tunnel path
  is a Noise IK session (`Noise_IK_25519_ChaChaPoly_SHA256`) end to end
  between phone and box; the phone pins the box's key from the pairing QR.
- **Push** content is sealed (ChaCha20-Poly1305) with a per-phone key agreed
  at pairing; Apple and the relay see a generic alert and ciphertext.
- **Peer-to-peer** sync between contacts' desktops runs over iroh (QUIC/TLS).

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
  at the box's role. It never holds the workspace key.
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

- An **account** (one per person; GitHub sign-in links devices to one
  account) certifies **device** keys. A `ligus serve` box can be certified as
  one of your devices; it can't certify itself.
- Membership is workspace-wide, kept on the workspace config log. Roles:

| Role | Can |
|---|---|
| **Owner** | everything an Admin can; the last Owner can't be removed or demoted |
| **Admin** | add, remove and re-role members; manage workspace runtimes (they carry credentials); revoke devices (a revocation or removal from anyone below Admin is ignored on every device); dearth mode and join approvals; path zones and the home beacon |
| **Contributor** | messages, proposals and votes, proposal sets, agents, skills, vaults, workflows, channels, hosts, leases; clear a chat lockdown (humans only) |
| **Viewer** | read; raise a chat lockdown |

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
  resolved gets no tools. See [the author gate](../concepts/security.md#author-gate).
- **Worker write access** is off by default:
  `LIGUS_WORKER_PERMISSION_MODE` must raise it, and then only Admins' messages
  drive write-capable turns there unless you lower `LIGUS_MCP_MIN_ROLE`.
- **Agent processes are sandboxed.** No agent process can read Ligus's keys,
  settings, database or `serve.token`, write outside its worktree, or reach
  the network except through a per-chat allow-list. See [the OS
  sandbox](../concepts/security.md#os-sandbox).
- **Phone and serve approvals.** A `ligus serve` box enrolled as your device
  signs as you, so until passkey presence lands it refuses votes, rejects and
  lockdown clears from the phone or any bearer caller (`403
  presence_required`), and every device's fold drops a vote such a box signs
  for you: approve on your laptop.
- **Agents** sign with their own host-certified keys, judged by a capped
  Agent tier that never votes, applies or administers. They act through the
  [Ligus tools](../addons/ligus-mcp.md): proposals only, behind a per-turn
  credential, with budgets and an audit trail. A host that can't certify a
  key signs for the agent (see
  [Known limitations](threat-model.md#known-limitations)).

## The 2.0 guards {#guards}

| Guard | What it does | Who can switch it | Details |
|---|---|---|---|
| **Sandbox envelope** | per turn: isolated worktree, protected dirs (`.git`, `.ligus`, `.hive`, `.claude`, `.vscode`, config files), egress policy, secret redaction in logged replies; only ever tightens | automatic | [The sandbox envelope](../concepts/security.md#the-sandbox-envelope-around-every-agent-turn) |
| **OS sandbox** | every agent process: Seatbelt (macOS) or bubblewrap + Landlock + seccomp (Linux); no reads of Ligus's data, keys or `serve.token`; writes only in the worktree; network only through the turn's egress proxy; write-capable turns refused where no sandbox exists | automatic | [The OS sandbox](../concepts/security.md#os-sandbox) |
| **Workspace trust** | repo-supplied runtimes and MCP servers stay inert until enabled on this device, per exact command | you, per device | [Trust this workspace](../concepts/security.md#workspace-trust) |
| **Dearth mode** | two Admins for new members; cross-account approval for new devices; agents limited to allow-listed MCP servers; human approval for protected (brood) paths | Admin | [Dearth mode](../features/guards.md#dearth-mode) |
| **Chat lockdown** | on an injection marker, a credential in a diff or an unrequested CI change: no network or MCP for that chat's agents, human approval on every proposal | raised by anyone (or automatically); cleared by a human Contributor+ | [Chat lockdown](../features/guards.md#chat-lockdown) |
| **Leases** | exactly one device drives a workflow run or the workspace coordinator role; signed heartbeats over relay presence; epoch fencing | Contributor (signed by the holder device) | [Which device drives a run](../features/workflows.md#which-device-drives-a-run) |
| **Ligus tools** | agents act through typed tools bound to one turn (an HMAC credential that expires with it): proposals only, never votes, applies or admin changes; budgets per turn; a device-local audit per call and a synced summary per turn | automatic; budgets per device (`LIGUS_MCP_MAX_*`) | [Ligus tools for agents](../addons/ligus-mcp.md) |
| **Exfiltration guard** | once a turn has read project files, MCP calls are refused | on by default; CLI can disable with `LIGUS_MCP_AFTER_READS` | [MCP servers](../addons/mcp.md) |
| **Hygiene sweep** | removes leftover worktrees (saving patches), marks un-appliable proposals stale | automatic | [Hygiene](../features/guards.md#hygiene) |

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
- [The 2.0 guards](#guards): the OS sandbox allows exactly one extra path, the
  turn's Ligus MCP socket. CLI agents run with a per-turn home, so they can't
  persist hooks or MCP servers into your real CLI config (C-9). On macOS the
  profile also denies Apple Events and the pasteboard (C-13).
- Implement on a command proposal runs its stored argv exactly, sandboxed and
  offline; the preview shows the same plan the runner uses.

**2026-11-13: core, host, chart and relay v0.2.0 review.**

- [Roles](#roles): revoking a device needs Admin, and both ingest and the
  trust roster ignore anyone below it. A removed member can't enroll
  themselves back; only an Admin's re-add lifts the removal.
- [The 2.0 guards](#guards): dearth mode holds devices awaiting their second
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
- Relay v0.2.0: a malformed `LIGUS_RELAY_TOKEN_PUBKEY` refuses to start;
  per-client limits use `LIGUS_RELAY_TRUSTED_PROXIES`, and `Fly-Client-IP`
  is trusted only on Fly; tokens can be confined to workspaces
  (`LIGUS_RELAY_WORKSPACE_SCOPE`); the chart's tunnel is off by default. See
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
- [The 2.0 guards](#guards): agent processes run in the OS sandbox with a
  per-turn egress proxy and an allow-listed environment; repository-supplied
  runtimes and MCP servers are inert until trusted on the device; the author
  gate applies on every host at the author's role when they posted. Turn
  measurements are device-local facts.
- [Phone and serve approvals](#roles): a delegated serve box refuses votes,
  rejects and lockdown clears, and folds drop its votes.
- Organization sign-in: [ligus-auth](../deploy/ligus-auth.md) issues
  short-lived relay tokens from your identity provider.
- Still open and listed in
  [Known limitations](threat-model.md#known-limitations) until the rest of
  Phase 0 fixes them: signed key rotations, the keyring on headless devices,
  keys in an OS keystore, passkey presence on admin events and phone votes,
  and hosts issuing agent keys (Track B); typed agent tools and the ACP
  driver (Track D).

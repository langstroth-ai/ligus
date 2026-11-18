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
  addresses, device public keys and labels, contact ids and the timing and
  size of drops into contact boxes, which tunnel hosts exist and when phones
  connect; when a device's presence slot changes (typing activity, but not
  which chat or who: the presence blob is sealed);
- drop, delay or replay ciphertext, or refuse service;
- with membership mode on, see the roster and invite metadata.

It **can't** read content, forge a member's events (they are signed, and
roles are checked by every device), or obtain a workspace key: keys are
exchanged out of band, and rotations are signed events on the config log,
sealed only to keys devices published there. It can't inject a key: keyring
rows on the relay are used only to read, never to seal. It can't pin a
device's sync with a body under an unknown key either (such bodies are
parked), and a flood of such bodies can't push out the ones waiting for an
authorized rotation (junk is evicted first). An invite to a co-member or a
new DM is sealed only to keys their devices published, signed, on a
workspace you share: the relay supplies no keys, so it can't substitute its
own. It can withhold, delay or replay contact records and drops, but a record
is checked against the safety code you got out of band and an older record
than one already seen is refused, and a drop is sealed. It can't compute
whether two people have a DM: rooms and invite ids are random. It never sees
a forge (GitHub, GitLab, ...) token: the client can't send one. It can't inject plaintext: a device that holds a workspace key skips
any body from the relay that isn't sealed. In the tunnel it can't read or
alter phone traffic (Noise, with the box's key pinned from the QR code). A
langstroth-auth token goes only to the relay the session was signed in for.

Each invite carries a fingerprint of the founder's account key, and a device
pins it when it joins, so a hostile relay can't substitute the founder's key.
Every device verifies and folds by the same rules whatever its environment:
`LIGUS_IDENTITY_MODE` is ignored since 0.1 (a set value logs a warning).

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
events are refused on every device (but see the backdated-frontier
limitation below). They can't post as another member: an event whose stamp
names an account its signing device isn't certified for is quarantined, and
the author gate gives it no role. After removal with key rotation they can't read new
traffic or attachments on any host, old-key writes from them are refused,
and a box enrolled for them (even one still hosting an agent) gets no new
key.

### A compromised serve or worker box {#compromised-box}

A `ligus serve` or `ligus worker` box is its own **service member**, never a
person's device. Whoever takes over one (its data dir, its process, its pod)
holds the workspace key, so they **can read the workspace** and write as that
box, at its role. What they **can't** do, on any device's fold:

- **Pose as the owner or any person.** The box signs only with its own account
  and device key. An event it stamps as someone else is quarantined (its
  device isn't certified for that account), and a box's stamp must say
  `service`, so it can't claim to be human either. It holds no account key of
  anyone's, so it can't certify a device for a person, and no passkey, so it
  can't confirm anything that needs one.
- **Rise above Contributor.** A box admission admits at most Contributor, and
  raising a service member higher is refused.
- **Approve for a person.** Its votes count at its role but never as a human
  approval, so dearth-mode protected paths, locked-down chats and an agent's
  own proposals still need a person. It can't decide a proposal set, answer
  an agent's question, clear a lockdown, or apply a change except to fulfil a
  person's request.
- **Cut or admit anyone.** It removes, demotes and admits no one, and issues
  no invite or admission; it revokes no device but its own.
- **Forge a phone's action.** What the phone signs itself (box admissions in
  0.1) the box only relays: it can withhold it, not alter or forge it.

The remedy is the one for any member: an Admin removes the box (or revokes its
device) and rotates the workspace key, and the box gets no later key. A box
never founds a workspace either: it syncs first and refuses to start without a
copy of the workspace, so a box pointed at an empty or withheld room can't
seed a competing config log naming itself Owner.

**Admissions.** A box admission is single use, expires within 7 days, admits
only a service member, and is bound to the box that redeems it (its device
signs the redemption). Admitted from a join request, it is also bound to that
box's account and sealed to its key. A bearer admission (the Kubernetes path)
is a secret until redeemed: see the known limitation below.

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
  a note; an automatic board pickup, whose text someone else filed, runs
  below every floor.

These reduce risk; they don't make an agent safe to run with broad
permissions on an untrusted repository. Runtimes Ligus can't confine from
outside (aider, pi, Hermes, Antigravity, Copilot) are refused whenever
egress is restricted, but run with their own permissions otherwise.

### Endpoint redirection (model gateways) {#endpoint-redirection}

Where an operator routes agents through an LLM gateway
([Deploying with an LLM gateway](../deploy/model-gateways.md)), the prize is
redirecting an agent's model traffic: to read prompts and code, or to
collect the gateway credential.

| Threat | Mitigation |
|---|---|
| A member points an agent at an endpoint they control | Setting a runtime's gateway or endpoint, and an agent's host or runtime binding, needs an Admin and passkey presence. A host with a gateway allow-list is exclusive by default: it runs agents only through gateways its operator listed. A gateway credential is attached only by the host, only for the URL the operator listed under that name. |
| An agent (prompt injection) rewrites its CLI's base URL or config | The CLI's only route to a model is the turn's egress proxy. The proxy adds the credential only on the gateway route, for the listed URL; the agent's allow-list holds no other model host, and NetworkPolicy opens only the listed gateways. |
| The agent reads or forges the credential | Credential files are on the sandbox deny list and are never in the agent's environment, argv or files. The proxy strips every credential header the agent sends before adding the gateway's. |
| A forged host record advertises a gateway | Host records carry names only. The host running the turn checks its own operator file; a name it doesn't list is refused. |
| DNS rebinding of the gateway's name to loopback or metadata | The proxy and the HTTP clients resolve and check addresses themselves and dial the checked address; NetworkPolicy refuses metadata ranges, and the chart refuses CIDRs that overlap them. |
| Token replay by the gateway | ServiceAccount tokens are projected, short-lived and bound to one audience per gateway, so a gateway can replay a token only to itself. |
| A token from another namespace or workload | The gateway checks issuer, audience and subject (`system:serviceaccount:<ns>:<sa>`); the docs say how. |
| Plain HTTP | Refused unless the operator sets `allowPlaintext`; never for mTLS. |

Residual risks: the gateway operator sees all model traffic, by design;
`allowPlaintext` without a mesh exposes bearer tokens to the node network; a
compromised serve or worker process (not an agent child) holds the gateway
credentials; a stolen token works for its own audience until it expires.

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

Stated plainly, as of 0.1.0. Each stays on this list until a release fixes
it.

1. **Secrets in the OS secret store, with a file fallback.** Account,
   device and key-agreement seeds, the workspace passphrase and epoch keys,
   relay and provider tokens and the langstroth-auth session live in the OS secret
   store: on macOS a dedicated keychain file inside the data dir, the Secret
   Service on a Linux desktop, `systemd-creds` on a headless Linux box.
   `settings.json` and `config.toml` hold only `{"set": true}`
   placeholders. On macOS the keychain's unlock password is a login-keychain
   item whose access list trusts only the Ligus binary: a release app by its
   designated requirement (Apple anchor, bundle id `ai.ligus.desktop`, team
   id), and it won't create the item unless it satisfies it; a development
   build or the CLI by its code hash, so each rebuild or upgrade of those
   asks once for your login keychain password. An unlock item created by a
   build before 0.1.0's review fixes keeps its original access list. If the
   keychain can't be used, macOS refuses to save secrets (Settings says why)
   rather than move them to files; `LIGUS_SECRET_BACKEND=file` is the
   explicit opt-in to `0600` files. Elsewhere, where no store works (Windows,
   a box without `systemd-creds`), Ligus falls back to `0600` files and says
   so in **Settings → Passkeys & keys**. Anything running unconfined as your
   OS user (not an agent, which the sandbox keeps out of the data dir) can
   still ask the Secret Service or, on macOS, prompt for the unlock item.
2. **An invite carries keys.** A `hivews1:` code carries the passphrase and
   every authorized epoch key the inviter holds, so the joiner can read the
   history. Treat it like a password and send it over a trusted channel.
3. **Agents run as your OS user, inside a sandbox.** Agent processes run in
   the [OS sandbox](../../ligus/concepts/security.md#os-sandbox) (Seatbelt on macOS,
   bubblewrap + Landlock + seccomp on Linux), which denies the data dir (key
   seeds, workspace keys, `settings.json`, `serve.token`), credential dirs and
   direct network. The sandbox shares your user and kernel: it is not a
   boundary against kernel bugs. A host without it (Windows, Linux without
   bubblewrap and user namespaces or Landlock, `LIGUS_SANDBOX=off`) runs no
   agent process at all: CLI agent turns at every tier, commands, pre-flight
   tests and stdio MCP servers are refused, and HTTP models answer without
   local tools. Windows is not supported for agent turns. On Linux a
   protected directory that doesn't exist yet is pinned by an empty
   read-only directory for the turn.
4. **Agent output signed by its host.** Hosts certify a key for each agent
   bound to them, and that agent's replies and Ligus tool effects are signed
   with it, judged by the capped Agent tier (never a vote, an apply or an
   admin change) and retired when the agent moves to another host. A host
   that can't certify a key (not yet certified, or the device-local Home)
   signs for the agent itself; that output, and every effect of the chat's
   primary runtime (stamped `runtime_effect`), is judged at the Agent tier
   too. Either way, a host's signature on an agent's output never gives a turn
   that output triggers the host user's tier: the author gate resolves an
   agent's message through its reply chain to the lowest role of the humans
   who started it ([the author gate](../../ligus/concepts/security.md#author-gate)).
5. **Invites carry every key, and first contact is trust on first use.** Chat events, attachment blobs, host liveness beats and
   workflow and coordinator lease beats follow signed key rotations on every
   host (readers take beats only at the newest authorized key). An invite
   code carries the passphrase and every authorized epoch key: anyone who
   sees a code reads the workspace's history up to that point, whether or
   not its single-use invite was redeemed; rotate after a code leaks. An
   invite to a co-member and a new DM are sealed only to keys published,
   signed, by devices certified for that account on a workspace you share.
   That is as strong as that shared workspace's binding of the account key
   (first registration wins there). When two workspaces disagree about the
   key, the one a founder pin names, else the earliest-pinned workspace's,
   stands; the other workspace is marked as disputing that person and
   nothing from it is used for them. Sealing is refused only when every
   workspace that registers a key for them disputes it. Someone you share no workspace with is anchored only by the
   safety code you compare out of band: skip the comparison and the first
   contact is trust on first use. Display names are not identity; an invite
   that names a co-member but carries another key is refused.
6. **Signed rotations rely on what a device folds.** A rotation counts only
   when an Admin's certified device signed it at the next version on the
   config log, and a device seals only under such a rotation; junk or
   unsigned keyring rows on the relay are never used. A device that has no
   founder pin for a workspace (joined without a pinned invite) adopts no
   rotation and keeps sealing under the passphrase
   key. A device that isn't a recipient of the newest rotation doesn't write
   until the next rotation includes it.
7. **Presence needs a passkey on every workspace.** Every workspace is on
   rules version 2 from its first event: membership and role changes, device
   revocation, key rotation, passkey removal, dearth mode, lockdown clears,
   path zones, workflow definitions, agent host or runtime changes, lowered
   gates and human-gated votes need a passkey assertion over the event inside
   its signed stamp, verified on every device. Passkeys are registered per
   workspace (on its config log), under the workspace's relying party
   (`ligus.ai` unless a self-hosted deployment set its own at creation). The
   desktop runs WebAuthn in your browser on that origin's `/presence` page; a
   compromised page there
   can't forge a device signature, but it could ask your authenticator to
   sign a different event during a gesture you started. The desktop checks
   the returned challenge is its own, and the sheet shows the first 64 bits
   of the digest; the page computes the same id from the challenge it signs
   (never from the link), so compare the two. A person who lost every
   passkey confirms with the recovery key's 24 words in the same sheet. An
   account's **first** passkey needs a grant, good for one registration: the
   founder's, from the workspace's first event, or an Admin's: being added as
   a new member, or a re-grant with the Admin's own passkey (**People → Allow
   new passkey**); an account that kept its recovery key re-registers with
   it. There is no trust on first use from a device. Once an account has
   a passkey, a new device of that account and an account-key rotation need
   its presence too (a stolen device and account key can't add devices or
   re-pin the key; a new laptop of yours shows **Confirm this device** in
   **Settings → Passkeys & keys** and counts once you confirm it), and any
   change of the rules (the relying party or the version, both the
   Owner's) needs presence, verified under the relying party in force before
   the change. An Admin can't clear the passkeys of a member above them or grant a role
   above their own. Removals, demotions and device revocations go strictly
   down the ranks: an Admin cuts only roles below Admin, only an Owner cuts
   an Admin, only the founder cuts another Owner, and nobody cuts the
   founder; so a removed member can't undo their removal with a backdated
   counter-removal.
   Until someone registers a passkey, their account is only as strong as
   their devices: a workspace founded from the CLI stays unsecured (nobody
   can join, nothing can be governed) until its founder adds a passkey from
   the desktop, and until then a thief of the founder's device key could
   register one first.
8. **A box is never you, so the phone acts through it only as the box.** A
   `ligus serve` box is its own service member: phone messages and votes sent
   through it are the box's, and its votes never satisfy a human gate. What
   only a person may do needs the phone's own device key and your passkey;
   in 0.1 only box admissions go that way, so approvals that need a person,
   answers to an agent's question and lockdown clears are made on the
   desktop. A bearer box admission (a Kubernetes Secret) can be redeemed by
   whoever holds it before the box does, and its expiry is checked against
   the redeemer's own signed time, so a holder could backdate a late
   redemption: keep it short-lived, delete the Secret once the box has
   joined, and remove any service member you didn't expect.
9. **Some agents still ask for actions in reply text.** Agents with tool use
   get the typed [Ligus tools](../../ligus/addons/ligus-mcp.md). Copilot and
   Antigravity run over [ACP](../../ligus/addons/acp.md); Codex and Claude Code do
   when their ACP adapter is installed, and Claude Code only with an API key
   set in Ligus and outside restricted-egress chats: a Claude subscription
   stays on `claude -p`. A turn whose ACP start fails falls back to the
   headless adapter. aider, pi, Hermes, and Codex and Antigravity on their
   headless adapters still use the `[[…]]` directives,
   which every host (desktop, worker, serve) now parses the same way. A
   directive can file only what the matching tool can.
10. **Agents can read every chat in the workspace.** `ligus_read_chat` reads
    any chat of the workspace the agent runs in (by design), so a prompt
    injection planted in one chat can reach agents working in another. Read
    results are marked as data, every read is in the device-local audit with
    the chat it read, and reads are budgeted (500 messages per turn). A
    locked-down or allow-listed chat can't be read from a chat with wider
    egress.
11. **A paused turn holds its host.** A turn waiting on `ligus_ask_human` or
    an ACP permission request keeps its process and chat until someone
    answers or its timeout passes (at most an hour; 10 minutes for permission
    requests). Stop the turn to release it. A restart cancels it; questions
    can't be resumed in 0.1.
12. **Sign counters aren't checked.** Synced passkeys report a signature
    counter of 0, so Ligus doesn't use it to detect cloned authenticators.
13. **No account-key rotation.** The core supports rotating an account key,
    but nothing in the app or CLI exposes it. A leaked account key means a new
    account.
14. **Removal is not erasure.** A removed member keeps everything they already
    synced. Rotation protects only what comes after.
15. **Roles don't limit reading.** Every member can read the whole workspace.
16. **The relay's open mode is open.** Without a token gate, anyone who knows
    the URL can store ciphertext on it and read ciphertext of workspaces whose
    ids they know. On an open relay that keeps state in memory, set
    `LIGUS_RELAY_MAX_STORE_BYTES` so writes can't exhaust its memory.
17. **The relay sees metadata** (above), including which contact ids get
    drops and when. The relay's admin
    API isn't in the open-source build, so per-person relay tokens need signed
    tokens (short-lived ones from [langstroth-auth](../deploy/ligus-auth.md), for
    example) or a restart to change an allowlist.
18. **The direct phone path is plain HTTP.** Use the relay tunnel or a
    tailnet, or a LAN you trust. Invite codes, code-host tokens and device
    sign-in codes are refused on it (tunnel or loopback only), but chat
    content and the bearer token still cross it.
19. **One `serve.token` for every direct-path client.** Revoking one direct
    phone means rotating it for all. Tunnel phones are revoked one by one.
20. **Revoking a device doesn't rotate the key.** A revoked device's new
    events are refused, but it still holds every key it had until an Admin
    rotates ([Rotate the workspace key](runbooks.md#rotate-workspace-key)).
    A removed member who writes with an old key is refused once a device
    knows of the removal; an event already accepted before that stays (the
    choice of what was accepted depends on arrival, like revocation).
21. **A linked account proves "the forge says so", nothing more.** A forge
    can list any key for any of its users, so it can make a link verify.
    Links never grant membership, roles or sealing trust. A verified status
    is cached on a device for up to 24 hours, so a key removed from the forge
    keeps a link verified that long. SourceHut and Tangled links from a 0.1
    pre-release are shown as self-asserted and never verified. Checking a link tells the forge
    that your device looked. See [Linked accounts](../../ligus/features/linked-identities.md#privacy).
22. **Pre-encryption history isn't shown.** A device with a workspace key
    never applies a plaintext body from the relay, so events a workspace
    stored on a relay before it had a key don't appear on devices that sync
    them later. Devices that already held them keep them.
23. **A shell-tier Claude Code turn holds your Claude account.** Ligus
    starts Claude Code without cloud routines, scheduled tasks, background
    monitors, push, your plugins, hooks or MCP servers
    ([details](../../ligus/concepts/security.md#claude-code-turn)), but at the
    *Edits + shell* tier and above the agent can start its own `claude`
    with other flags, and your login is reachable from the sandbox. That
    `claude` can create a cloud routine that runs on Anthropic's side; the
    egress proxy can't tell it from a model call (same host). Cron,
    background tasks and workflows stay off through the inherited env. Use
    a non-shell tier for chats whose input you don't trust. Copilot and
    Antigravity still load your own MCP servers and plugins (no strict
    switch), and Codex over ACP isn't confined to Ligus's MCP servers yet.
24. **A cut is only as complete as its author's view.** A removal,
    demotion or revocation carries a watermark of the member's (or
    device's) events its author held, and every later event of theirs is
    judged as of the cut, whatever config position it names, in any
    delivery order. Two consequences, both by design (review P-A2e,
    P-A2f):
    - **The watermark is its author's word.** Peers can't check it. A
      remover who names too little (an empty watermark, or old heads) has
      the member's later-looking history judged as of the cut: on the
      config log their uncovered acts (members they added, roles they
      changed) are denied, and on chats their uncovered posts are judged
      as a non-member's. Their own keys, certificates and passkeys stand,
      and so do a demoted member's lockdowns: a demotion never lifts a
      lockdown (only a human Contributor's clear, with presence, does). A
      removed member's or revoked device's uncovered lockdowns are denied.
      Ranks bound who can do this: a cut goes strictly down the ranks of
      the roster at its own position, so only someone above the member
      there can, and a backdated counter-cut by the subject is denied. Only
      a later, allowed role change (the founder promoting the member) lets
      them cut the earlier author afterwards.
    - **Chats the remover never synced.** Events the remover's device
      doesn't hold (a chat it never synced) are cut too, so sync before
      removing someone whose recent history matters.

    Leaving yourself, demoting yourself and a device revoking itself are not
    cuts; what such a member could backdate afterwards they could have done
    before.
25. **Equivocation is caught where both copies meet.** Event ids are
    chosen by their author, so a signer can publish two different envelopes
    under one id. A device that holds both denies both, whatever order they
    came in, and coverage never passes through that id; but a device only
    learns of it when the second copy reaches it (the relay keeps the first
    one it saw), so devices can disagree until both copies have reached
    them.
26. **A human click runs others' text at the clicker's tier.** Some
    actions place text someone else wrote into a prompt that runs as the
    person who clicked: **Send back to revise** (the review thread),
    **Implement** on an approved instruction proposal (the agent is sent
    the proposal's instruction text), and picking up a board item by hand
    (its title).
    That is a prompt-injection surface at your role; read what you are
    sending before you click. Automatic board pickup doesn't have this
    problem: it runs below every floor.
27. **LSP servers and the terminal panel run unconfined.** They are your
    own tools, started for you, and run outside the agent sandbox with your
    OS user's access, by design. A repository's language-server config or a
    command you paste into the terminal runs with everything you can reach.

## Out of scope {#out-of-scope}

- A compromised OS account or machine that runs Ligus.
- Model providers' and MCP servers' handling of what turns send them.
- Availability against a determined denial of service on your relay.

## Reviewer findings {#reviewer-findings}

Security review findings that add to or change this threat model are listed
here with their status. The review of November 2026 covered the core, the
host (desktop, CLI, `ligus serve`), the Helm chart and relay v0.2.0.

### Fixed in 0.1.0 {#findings-fixed}

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
| Host | A Claude Code turn inherited the user's whole Claude Code setup: cloud routines and remote triggers (run on Anthropic's side under the user's account and connectors), cron, background monitors, push, cross-session messaging, workflows, user and plugin MCP servers, hooks, plugins and skills | Every turn gets a built-in tool allow-list, the out-of-turn tools denied (by flag, in the turn home's settings, and by env), strict MCP with only Ligus's and the chat's servers, and no settings, hooks, plugins or skills unless the workspace opts in ([details](../../ligus/concepts/security.md#claude-code-turn)) |
| Host | Ligus's own MCP tools were refused in non-interactive Claude Code, Copilot and ACP turns (nobody to approve them) | Pre-approved: `--allowedTools mcp__ligus`, `--allow-tool=ligus`, and ACP permission requests for Ligus tools |
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
| Keys (Track B review) | Attachments and host liveness beats stayed on the passphrase key; a removed member's box still hosting an agent got every new key; a stranger could deliver the key a DM's second side adopted | Blobs and host beats follow the keyring; rotations seal only to devices of current members; a DM key is adopted only from the DM's friend |
| Presence (Track B review) | The passkey page showed a 32-bit digest id taken from its link; re-owning another member's host skipped presence; an Admin could strip an Owner's passkeys; the recovery key had no way to be used; dearth mode was read from an unpinned projection | A 64-bit id the page computes from the challenge; host takeovers need presence; removals only at or below the remover's role; Use recovery key in the confirmation sheet; dearth mode follows the founder pin |
| Presence (Track B residuals) | Under rules v2 a device-key thief could certify new devices, rotate the account key (refused by the fold but still applied to the rosters) and, once an account's credentials were cleared or before it had any, register a "first" passkey; switching to an unknown rules version needed no presence | New devices and account-key rotations of an account with a passkey need its presence, and both rosters apply only allowed ones; a first passkey under v2 needs an Admin's grant; every switch to v2 or later needs presence |
| Keys (Track B residuals) | Lease beats stayed on the passphrase key; a flood of junk bodies could evict parked bodies waiting for an authorized rotation (B-11); invite and DM codes were sealed to whatever keys the relay directory listed | Lease beats follow the keyring; parked bodies are ranked and junk goes first; codes go to signed directory keys, or after an Unverified recipient warning |
| Secrets (Track B residuals) | The keychain unlock item trusted whichever process created it; an unusable keychain silently moved secrets to plain files; the passkey loopback read unbounded lines and its nonce was on the browser's command line (B-12) | An explicit access list for the Ligus binary (designated requirement for release apps); macOS refuses without `LIGUS_SECRET_BACKEND=file`; the loopback caps what it reads and the browser opens a private redirect page |
| Relay (E-5) | The langstroth-auth access token was sent to any configured relay, an invite's included | Only to the relay origin its session was signed in for |
| Host (C-9) | An agent could write hooks or MCP servers into `~/.claude`, `~/.codex` and the other CLIs' config, which ran unsandboxed on the user's next CLI start | CLI agents run with a per-turn home seeded with the top-level config files; their real config dirs aren't writable, and only refreshed sign-in files are copied back ([details](../../ligus/addons/acp.md#cli-home)) |
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
| Sandbox (Track C/D residuals) | The Seatbelt profile protected `.git`, `.claude`, `.vscode`, `ligus.config.toml` and the other protected names only at the worktree's top level and only in their exact case | The profile also denies writes to each protected name at any depth and in any case (`sub/.claude/`, `.CLAUDE/`). The profile stays default-allow in 0.1; see [the OS sandbox](../../ligus/concepts/security.md#os-sandbox) for why and what that leaves open |
| Agents (Track C/D residuals) | `maxSpendUsd` only refused Ligus tool calls, counted only ACP `usage_update` (whatever its currency, and only upward deltas), and never stopped a turn | Reaching the budget ends the turn (ACP `session/cancel`, API streams closed); ACP reports must be USD and non-decreasing or a budgeted turn ends; Anthropic, OpenAI, OpenRouter and Azure turns are charged from the API's token usage at list prices (unknown models at the highest rate). An agent that under-reports over ACP is still only held to what it reports |
| Agents (Track C/D residuals) | Budgets were per turn only, so one human message could fan out through hand-offs to about 24 turns of 8 proposals each; ACP permission escalations asked people without counting against `maxAsks` | A cascade budget (tool calls, proposals, hand-offs, questions) is shared by every turn descended from the same human message on a device; permission escalations count as questions and are denied past either question budget |
| Sandbox (C-12) | On Linux the egress proxy's token was an argument of the sandbox helper, readable by every user on the host with `ps` | It reaches the helper in its environment (`LIGUS_SANDBOX_PROXY_TOKEN`, readable only by your user), and the helper removes it before running the agent |
| Sandbox (C-10) | Under bubblewrap an agent could `connect()` to any host unix socket visible through the read-only `/` (`docker.sock`, an ssh-agent) | Seccomp refuses `socket(AF_UNIX)` to agents, tools and pre-flight runs (the Ligus MCP server is reached through a loopback forwarder in the sandbox's network namespace), and known host sockets are masked. Verified by `sandbox_denies_unix_socket_connect`, which runs only on Linux |
| Agents (Track C/D residuals) | `ligus_read_proposals` (up to 50 proposals with their bodies) wasn't charged against `maxReadMessages` | Each proposal read counts like a message |
| Agents (Track C/D residuals) | ACP `fs/*` never applied the plan's read denials (the check could not fire) | A plan-denied path inside the worktree (a data dir or token file there) is refused to the host's own reads and writes |
| Agents (Track C/D residuals) | Turn claims were keyed by the trigger message alone, so when one reply handed off to two agents on two devices, the second receiver never ran | Hand-off receivers claim (trigger, agent); user-message turns keep the plain claim, so older devices still coordinate |
| Release | `scripts/release-matrix.sh` passed API keys as curl arguments, visible in `ps` | Headers go through a private file |
| Host (authority) | The author gate read only the immediate trigger's author, and a message every event of which this device signed as its user cleared every floor. An agent reply the host signed for an agent without its own key (the device-local workspace, no agent vault, a key that couldn't be certified) counted as the host user's own message, so a Contributor's request handed to another agent, or an @mention cascade from it, ran at the host's Owner tier. Workflow stage prompts, posted as the driving device's user, ran at that user's tier whoever started the run | An agent's message has no role of its own: every host resolves it through its reply links (hand-offs, cascades) to the lowest role of the humans behind the chain, caps a peer's agent output at that peer's role, and runs a chain with no human read-only. A stage prompt is linked to its run and judged at the lower of its poster's and the run initiator's role. Verified by `handoff_chain_author_role_is_min_of_chain`, `mention_cascade_never_raises_tier`, `host_signed_agent_output_carries_no_role`, `agent_signed_reply_resolves_to_the_chain_human` and `workflow_stage_role_is_the_run_initiators` |
| Sandbox (unconfined hosts) | Without an OS sandbox only write-capable CLI turns were refused: read-only CLI turns ran with nothing but an env allow-list and could read `hive.db` (not encrypted at rest), keys and tokens; `run_command`, approved commands, pre-flight tests and stdio MCP servers ran unconfined at any tier | Every agent process is refused at any tier, with a note naming the platform and the fix; HTTP models get no built-in file tools and run as plain chat. Verified by `unconfined_refuses_read_only_cli_turns`, `unconfined_refuses_tool_purpose` and `unconfined_allows_toolless_api_chat` |
| Core (identity binding) | Ingest checked a stamp's account against the signing device only when the stamp carried one, so a member's device could stamp another member's actor id (their account id) with no account: the fold judged a `MemberRoleChanged` as that member (a Contributor could promote themselves to Owner) and the author gate gave the turn their role | A stamp whose actor id is a known account the signer doesn't sign for is quarantined as impersonation, and held from an uncertified device. The author gate counts a member with an account only when the stamp names that account and the signer is certified for it and unrevoked at the stamped position. Verified by `a_stamp_naming_another_member_without_their_account_gets_no_role` and `chain_loops_and_overlong_chains_fail_closed` |
| Host (board pickup) | Automatic board pickup posted the item's task (a proposal's or failed stage's title, written by whoever filed it) as the host's own user, so the author gate's own-user rule ran it at Owner: anyone could title a proposal with instructions an Owner's desktop then ran at full tier | An automatic pickup is posted as a runtime effect with no role, below every floor; a manual pickup is unchanged. Verified by `auto_pickup_is_not_the_host_user_speaking` |
| Core (chain workspace) | The trigger chain judged each event on the config log of the workspace that event named, so an event in a chat stamped with another workspace its author owned was judged as Owner there; the chain's config cache answered every workspace with the first one it loaded | Events stamped with a workspace other than the chat's fail the chain closed, a workflow run's first record must name the chat's workspace, and the cache is keyed by workspace. Hardening: such an event doesn't fold into the chat today. Verified by `an_event_stamped_with_another_workspace_fails_closed` |

### Open {#findings-open}

These are in [Known limitations](#known-limitations) and stay there until a
release fixes them: invite codes carrying every epoch key, directory keys
for people you share no workspace with (unverified, sealed only after a
warning), a CLI-founded workspace staying unsecured until its founder adds a passkey,
devices without a founder pin keeping the passphrase key, and ingest-time
decisions that depend on arrival order, the reach of a removal's watermark (its
author's word, and chats the remover never synced), equivocations seen only
where both copies meet, human
clicks that run others' text at the clicker's tier, and unconfined LSP
servers and terminal. On Kubernetes, DNS tunnelling through
the cluster resolver remains possible (see
[Kubernetes](../deploy/kubernetes.md#residual-risks)).

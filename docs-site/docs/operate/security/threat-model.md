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
`LIGUS_IDENTITY_MODE` is ignored (a set value logs a warning).

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
the author gate gives it no role. Nor can they (or anyone holding the key)
erase someone else's event by publishing another envelope under its id: only
the same signer's two copies are an equivocation (see
[Equivocation](#known-limitations), item 25). After removal with key rotation they can't read new
traffic or attachments on any host, old-key writes from them are refused,
and a box enrolled for them (even one still hosting an agent) gets no new
key.

<a id="account-keys"></a>**They can't silence another member through their
account key.** An account id isn't derived from its key, so anyone who can
write to the log can publish a registration of their own key under someone
else's account id. Such a registration counts only when its signing device
published its own certificate for that account under that key, and the
account's first word on the log fixes its key: the device that signed the
founder's seed, or the device that signed the account's own admission (its
self-enrollment through an invite or a box admission). Any other key is
refused, wherever it sorts (backdated before the owner's registration or
admission too), attributed to the device that signed it, and disputes
nothing: the owner's events keep folding. A forged rotation (one the
account's current key didn't sign) is refused the same way. The account
fails closed (its events are held, never applied) only when its own key
material asserts two keys: its own device registering another key, or a
key it rotated away from designating a second successor (a copy of a
rotation the account already made changes nothing). Until the anchor
device's own certificate is on the log, the account has no key there, so a
registration made in advance doesn't take its place. A founding seed signed
by anyone but a device of the founder's pinned key never joins the roster,
so nobody can anchor the founder to a device of theirs. A person invite
carries a redeem key whose secret rides only in the invite code: the
invite's id is on the log for every member, but a redemption counts only
with that key's signature over the workspace, the invite, the new member and
the redeeming device, so nobody can take someone's invite, or the anchor
that comes with it. Limits: an invite issued before redeem keys is still
redeemed by whoever's redemption sorts first; and an account an Admin added
directly has no anchor, so a forged registration whose signer also
certified itself under that key still disputes it, fail closed (no Ligus
client adds an account that way: the desktop's Add member writes a
placeholder with no account, so this is left to older logs).

<a id="held-events"></a>**They can't stall your sync with events nobody can
judge.** An event a device can't judge yet (a device not certified for the
account it claims, an account not admitted yet, a disputed account key)
waits in a device-local held table instead of holding the sync cursor, so
events whose certificate or admission never comes don't stop anything after
them. Held events are judged again by the same gate whenever the roster
changes and every 10 minutes, and one released later folds exactly as if it
had arrived after what it waited for. The table is bounded (1,000 events per
signing device and workspace, 10,000 events and 32 MiB per workspace,
50,000 events and 128 MiB per device, 30 days); past a cap the heaviest
signer gives way only to one holding fewer, so a later flood can't push out
what already waits. A held event dropped by a cap isn't lost: the relay
still has it, so the device remembers its sequence (no body) and, once what
it waited for arrives (its signer now signs for the account it claims),
fetches it again and judges it like any other, at most three times. A flood
from devices nobody certified is never fetched again. `ligus doctor` and
Settings > Team sync show how many wait and how many were given up (past
100,000 remembered per workspace, three fetches, or 30 days).

### Workspace repositories {#repositories}

[Workspace repositories](../../ligus/features/repositories.md) are signed
config events; checkouts and their paths never leave a device.

- **A malicious Admin, or a compromised one**, can add a look-alike
  repository (`github.com/zazu1am/wefit`). Adding and removing need the
  Admin's passkey presence; Settings shows the full canonical name and flags a
  repository whose name reads the same as another's once common ASCII
  confusables are folded (`1`/`l`, `rn`/`m`, `0`/`o`, dashes, case), or whose
  claimed root commit equals another's. A repository entry
  makes no device fetch anything: a clone is a person's act on one host, from
  that host's own code-host connection, with the URL rebuilt as `https://`
  from the canonical (never a string from the event) and the address checked
  again and pinned right before git runs; git follows no redirect, so the
  server at the pinned address can't send the clone to another host. An entry can't create a connection
  or mark one internal.
- **A member or an agent** can't add, remove or rename a repository, and an
  agent (or a runtime effect) can't set a chat's repository or pin its base.
  A proposal's repository comes from the chat when it is filed and never
  changes; its base commit is in the vote, so re-basing it clears the votes.
- **A host's coverage** rides its sealed presence beat, and the host signs
  the report with its own device key. A reader uses a report only when the
  signature verifies against that device as the workspace roster has it
  (certified, not revoked, not a phone, acting for a current member), so a
  member who holds the workspace key can't report another host's coverage:
  a beat it seals for that host keeps its liveness and loses the coverage.
  Coverage stays advisory: the host that runs a turn checks its own
  confirmed checkout (by remote, without `insteadOf` or includes) before
  every turn and refuses otherwise.
- **Driving the box's repositories from a phone** (clone by name or by id,
  rename, picking a chat's repository, rescan, confirming a found checkout)
  spends the box's own code-host access, so only the box owner can: a phone
  certified to the account of the Admin who admitted the box, over its own
  pairing, or the operator's `serve.operator.token` when the box listens on
  loopback. The phones' `serve.token` is refused everywhere: every paired
  phone can fetch it, and a reverse proxy on the same host puts phones on
  loopback too. Off loopback the operator token is refused as well. Anyone else gets `owner_required`. A request carried on the log never
  starts a clone.
- **A hostile repository** found by a scan is never used until a person
  confirms it on that device; its hooks, `core.fsmonitor`, submodules and
  file-protocol URLs stay off (every git call runs with those overridden),
  and its runtimes and MCP servers stay inert until trusted. A marker it ships
  in its work tree means nothing: the managing-host marker lives in the git
  directory.
- **Applying** builds the commit in a scratch index or worktree and moves the
  chat branch only by compare-and-swap, so a conflict or a moved branch
  applies nothing.
- **Ledger replay** reads `Ligus-Proposal` and `Ligus-Revert` only from a
  commit's trailer block (never its subject, which carries the proposal's
  agent-written title) and only on commits since the chat's pinned base. A
  host whose chat branch wasn't cut from the pinned base refuses the chat's
  turns instead of replaying onto a different base. An applied proposal's
  content can't change afterwards (a re-upsert that would is refused), so a
  replay only ever applies the approved bytes. A host fulfilling a person's
  "Apply on" request uses the same checkout check as a turn.
- **A chat's pinned base** can't be moved by any member: the first pin wins
  (switching the chat's repository away and back keeps it), and moving it
  takes an explicit re-pin by an Admin or the chat's creator, from their own
  device, with presence under rules v2, naming the pin it replaces. A host
  follows a re-pin only by rebuilding the chat branch from the ledger, and
  refuses rather than drop a commit that isn't an applied proposal.
- **One managing host per checkout** also binds work that doesn't claim one:
  hygiene never sweeps, and a chat without a repository never runs or
  applies in, a checkout another host on the machine manages.
- **Desktop actions on repository text.** "Ask the agent to rebase" quotes
  git's conflict output as data in a fence it can't close and keeps the
  agent's title on one line, so a patch's file names can't pose as your
  message. "Open in editor" hands the OS opener one absolute path to a real
  folder inside the checkout (never a link a repository planted, never a
  macOS package it would launch; on Windows one quoted argument, so
  `explorer` can't read a comma as a switch). "Clone into…" never follows a
  redirect and names the folder from `[a-z0-9-]` only.
- **A remote URL** with `#`, `?` or a backslash has no identity: git connects
  to the host before them, so `https://evil.example#@github.com/o/n` never
  reads as github.com's repository.
- **Limits:** coverage is self-reported: a host can sign "present" and
  then refuse its turns. Liveness on the beat isn't signed, so a member can
  make a dead host look online for a while; the dispatcher still hands a
  turn whose holder hasn't answered over after two minutes. A replayed old
  signed report counts on a reader that restarted until the host's next
  beat. A person who
  writes a commit with a real `Ligus-Proposal` trailer in a chat's worktree
  makes that host's replay skip that proposal there. The repository list is
  visible to every member, Viewers included.

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
  own proposals still need a person. A vote counts as a person's only when a
  device certified for that person's account (not revoked) signed it: the
  fold checks this on every device, on top of ingest quarantining an event
  whose stamp names another account than its signer's. It can't decide a
  proposal set, answer an agent's question, clear a lockdown, or apply a
  change except to fulfil a person's request naming it.
- **Cut or admit anyone.** It removes, demotes and admits no one, and issues
  no invite or admission; it revokes no device but its own.
- **Forge a phone's action.** What the phone signs itself (box admissions and
  the [phone actions](../reference/serve-api.md#phone-actions): post, vote,
  answer, clear a lockdown, Apply on ‹host›, withdraw) the box drafts and
  relays. A client that runs the preimage check before signing
  (`check_phone_action_preimage`) signs only exactly the action its person
  asked for, in this workspace and log, stamped as that person for that
  phone, with the passkey assertion the phone itself produced (or none): the
  box can withhold or delay an action, choose the fields that grant nothing
  (ids, its frontier and chain link, the log head it counts from), and show a
  misleading preview, but it can't alter or forge what is signed. The clock
  is no longer the box's to choose: an event's lamport is exactly one past the
  log head its stamp names, which every fold checks (and the phone before it
  signs), so an action the box withholds sorts where it was drafted, before
  anything written after it, unless the box first writes and publishes a
  chain of its own events as long as the gap it wants to jump. A delayed
  lockdown clear lifts only the raises it names, never one raised since
  ([Lamport bounds](#lamport-bounds)).

The remedy is the one for any member: an Admin removes the box (or revokes its
device) and rotates the workspace key, and the box gets no later key. An
honest serve box that sees its own device revoked also cuts every phone
pairing it holds and rotates its direct-path token, so phones stop sending to
it; a compromised one needn't, so re-pair phones with a box you trust. A box
never founds a workspace either: it syncs first and refuses to start without a
copy of the workspace, so a box pointed at an empty or withheld room can't
seed a competing config log naming itself Owner.

**Admissions.** A box admission is single use, expires within 7 days, admits
only a service member, and is redeemed only with the signature of a redeem
key whose secret rides in the admission code alone, over the workspace, the
invite, the new member and the redeeming device. The invite id is on the
workspace log for every member, so another member (or a sock puppet of
theirs) can't redeem it in the box's place. Admitted from a join request, the
code is sealed to the box's key-agreement key, and the fingerprint the Admin
compares covers the box's account, device key and key-agreement key, so a
request swapped in transit shows a different fingerprint. Every time on the
log is its author's choice, so expiry alone can't stop a holder of the code
from backdating a redemption: an Admin's device closes each admission once it
sees it redeemed (naming that redemption) or once it lapses (naming none),
and every other redemption of it is then refused wherever it sorts. The
redemption must be signed by the very device its proof names (checked when
it arrives, and again on every fold once that device is certified), and the new member's record names that
device's own account, bound admission or bearer, so a box can't join under a
person's account; nor can it change its kind, account or identity later by
re-adding itself. Closing is an Admin's call like issuing: an Admin can close
an admission to keep a box out (a closure that names no real redemption of
it admits no one through it and changes nothing else), and two Admins'
closures that name different redemptions admit neither. The box forgets the
redeem key once its redemption is written. See the known limitation below.
A person's invite works the same way: it names a redeem key whose secret
rides only in the invite code, and the redemption carries that key's
signature over the workspace, the invite, the new member and the redeeming
device ([account keys](#account-keys)).

**Apply on a host: one applier.** A person (desktop or phone) asks one named
host to apply an approved proposal (`proposalApplyRequested`). The fold, on
every device: allows one open request per proposal; lets only the named host
fulfil it (`proposalApplied` naming the request, signed by that host, on the
base the person saw when they named one), and only while the request is open,
so a retry or a second host is refused; refuses a person's own apply while a
request names a host; lets only the named host report a failure
(`proposalApplyFailed`), and only the requester or an Admin, a person through
their own device, withdraw a request (`proposalApplyWithdrawn`). A box never
asks for an apply and never applies on its own approval. The host claims the
request in its own store before touching git, judges its report as the fold
will first, applies only on a clean worktree (an agent turn's uncommitted work
never rides along), and commits with a `Ligus-Apply-Request:` trailer, so a
restart reports the commit it already made instead of applying again; a
trailer is only a hint (an agent can write one), so recovery reports a commit
only when its tree is exactly the approved patch on its parent. A request
never names a phone, which would hold the single applier slot forever.
Residual: a withdrawal that sorts before a host's report, but reached the host
after it committed, leaves that commit on the chat's branch while the log
says withdrawn (the host only takes it back when it sees the refusal as it
writes).

### Prompt injection and hostile content {#prompt-injection}

Content an agent reads (a web page, an MCP result, a file in an untrusted
repo, a teammate's message) can try to steer it. Defences:

- edits land in an isolated worktree and come back as proposals; nothing
  reaches your working tree without a human **Implement** or a person's
  **Apply on ‹host›** request (there is no auto-apply);
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

### Shared files {#shared-files}

Shared files are one of the easiest ways for an intruder, or a compromised
member device, to spread malware to a whole workspace. Files are end-to-end
encrypted, so the relay can't scan them: every content check runs on
members' devices ([Sharing files](../../ligus/features/attachments.md)).

| Threat | Defence |
|---|---|
| A member shares a program, script, macro document, disguised type or archive bomb | Sharing is off until an Admin sets the attachment policy (Admin and passkey presence; the recommended policy blocks programs, installers, disk images and scripts). Every device detects the type from the content, not the name, and refuses a blocked type on receipt too. Nothing opens or previews automatically; risky types need a second confirmation naming the sender; saved files carry the OS quarantine marking (Gatekeeper, SmartScreen). Archives are inspected without extracting anything, and a likely bomb isn't saved. An optional local scanner can refuse the save. |
| A known-bad file keeps circulating | An Admin blocks its SHA-256 (no presence needed: it only narrows). Every device refuses to share, fetch or save it, including copies shared before the block. |
| A stolen or revoked device, or a removed member, keeps posting files | A share is a signed chat event judged by the workspace fold at its author's position: a revoked device's later events never fold, and a removed member's are judged as of the removal (not a member). Removing a member can also delete everything they shared. |
| An agent shares a file, or is tricked into it | Agents never sign a share. With the policy's agent switch on, an agent's file is filed as a proposal; it is shared only after a person approves it, by the host that produced it, and every device checks the shared file is the approved one (same hash, size, name and type). |
| The relay swaps, truncates, replays or withholds a file | The signed reference binds the file's SHA-256 and size; a download that doesn't open to exactly those bytes is refused and nothing is kept. Withholding can't be prevented: the card says the file isn't available. |
| Filename tricks (right-to-left override, double extensions, traversal, reserved names) | Names are cleaned when shared and checked by the fold: one path component, no control, bidirectional or invisible characters, no leading dots, Windows device names prefixed, at most 200 bytes. A double extension (`report.pdf.exe`) is flagged. Saving never overwrites a file or follows a symlink and never sets an executable bit. |
| A preview runs code | Previews exist only for PNG, JPEG, GIF, WebP, BMP and plain text, rendered in an empty-sandbox frame whose policy blocks every load. SVG, HTML and PDF are never previewed. |
| A shared file carries a prompt injection | Agents get shared files only as delimited untrusted data (text content up to 64 KB, otherwise only a description), never as instructions and never as a path they could run. An agent can't offer a file outside its worktree, through a symlink or through a hard link. |
| A member uses the relay as free storage or a distribution point | The relay authenticates uploads, mints random non-enumerable ids, refuses past per-file, per-workspace, per-member, per-tenant and relay-wide quotas and an upload rate, and expires files (30 days by default). See [File attachments on the relay](../networking/self-host.md#file-attachments). |
| A path in someone else's message opens something on your device | A device-local `[Attached: <path>]` marker becomes a clickable chip only in your own messages; in anyone else's it is text. |

Residual risks:

- **Metadata.** The relay sees each file's size, when it was uploaded and
  fetched, and by which token or address.
- **Policy before content.** The fold can only judge what a share declares
  (its name, size and hash); the content checks run on each receiving device.
  A member on a modified client can upload any bytes; receivers still refuse
  them by content.
- **Relay deletion is best effort.** A deleted or expired file stops being
  offered on every device once the signed deletion folds, but a relay that
  ignores deletes keeps its ciphertext (which it can't read). A relay that
  binds each blob to the token subject that uploaded it deletes it only for
  that subject or a relay Admin; an upload with no token identity (an open
  relay tagging by address) stays deletable by anyone with write access who
  knows its id. When the relay refuses an Admin's delete of someone else's
  file, the file is still hidden everywhere by the signed deletion and the
  relay expires it at the end of its retention.
- **A policy change applies to files already shared.** A share is judged at
  the position its author stamped, and an author may name a position from
  before a policy change, so the fold can't stop such a share. Every device
  therefore also refuses to fetch, preview, save or give an agent a file the
  policy in force refuses (attachments off, its type blocked, over the size
  cap), whenever it was shared.
- **Attachment ids are bound to the sharing device.** A share's id is derived
  from the signing device and the relay blob (an agent offer's from its host
  and the proposal), so no member can claim someone else's file id first and
  suppress it.
- **Forged copies.** Until the same-signer-only equivocation rule lands, a
  member who copies a share's event id under their own signature makes both
  copies drop out (the share disappears for everyone); this holds for every
  event kind, not only shares.
- **Archives.** Zip and gzip files are inflated into a counter (nothing is
  written) to judge what an extractor would unpack, including names from
  Unicode Path fields and zips or gzips nested up to three deep. Entries that
  are encrypted or use another compression, and other archive formats (7z,
  RAR, tar, xz, …), can't be looked inside: they are flagged and need the
  second confirmation, but the types inside them aren't checked against the
  policy.
- **Saving** writes the verified bytes straight to the new file (marked
  first); if Ligus is killed mid-write, a partial, marked file can remain.
- **Local file markers.** A `[Attached: <path>]` marker resolves to a local
  file only in a turn the person wrote themselves, never in an agent's reply
  or a runtime effect signed under their identity. The chat view still shows
  a chip for a path string the person used in one of their own messages
  wherever that string appears.
- **Deleting a message doesn't delete its files.** Delete the file from its
  card, or remove the member and their files.
- **Quotas on an open relay** count per token subject, or per address when
  there is no token. Per-member quotas are on by default (128 MiB and 500
  files per workspace).
- **Linux** has no quarantine marking to set.
- **Scanning is optional** and depends on the scanner the person installed.
- **The phone app doesn't show shared files yet**, and `ligus serve` and
  worker boxes can't keep agents' file offers.
- **Older builds** without attachment support show the marker as text and
  don't fetch anything.

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

### A member spending someone else's credentials {#agent-use}

An agent's turn runs on its owner's device or its host worker, on their API
key, subscription, local model or the worker's runtime. The prize is
getting those turns run without the owner's consent, or past the owner's
budget. See [Who can use an agent](../../ligus/concepts/agents.md#who-can-use-an-agent).

| Threat | Mitigation |
|---|---|
| A teammate `@mentions` or messages your personal agent | Personal agents are "Only me" by default. The device or worker that would run the turn checks it before it claims the turn or calls a model, and answers with a short refusal instead; nothing is spent or counted. |
| Routing around it through another agent, a hand-off or a workflow | Every person behind the turn must be allowed: the chain is walked over the signed envelopes (the author gate's walk), including a foreign agent key's host member and a workflow run's initiator. A chain with no traceable person runs only on an agent open to everyone. |
| The sender's own device answers after the owner's device is silent (the deferred fallback) | The fallback checks access like every other path and refuses; it never runs someone else's personal agent on the sender's device. A refusal answers the trigger, so no device picks it up later. |
| Opening someone's agent up (setting `access` to everyone, or raising its budgets) | Only the agent's owner can change those settings (an Admin on a shared agent), whatever another member's role: every device's fold drops anyone else's change, including through a chat-level copy of the agent or a whole-roster replace (property test `agent_access_fold.rs`). Under rules v2, opening an agent to everyone also needs the owner's passkey presence, so a stolen device key alone can't open it. |
| A forged hand-off card (a member records a tool hand-off to your agent pointing at your own message) | The card's signer counts as a person behind the turn, so the card can't borrow your message's author. |
| Asking past your cap through another member's agent | The turn is counted against the person who asked, and every non-owner person behind it must be under the cap. |
| A teammate's `@ligus` on your device | The chat's primary runtime answers only its device's person in a shared room unless they open it (Settings → Privacy); `ligus agent` likewise. |
| Spamming mentions of an agent you can't use | One refusal note per member per agent every five minutes; the rest are silent. |
| A teammate drains your subscription's plan window | Their direct mentions of your agent rank as background work on your desktop and pause first as the window fills; your own direct turns always run. |
| Overspending | A per-member daily turn cap (50 by default on a personal agent) and an optional daily estimated-spend cap, counted where the agent runs. |

Residual risks:

- **Counting is per device.** Each device or worker that runs an agent
  counts in its own database; an agent its owner runs on two devices can
  be used up to the cap on each.
- **One turn overshoots.** Caps are checked before a turn starts; the turn
  that crosses the daily spend cap finishes. Spend is an estimate from
  reported token usage; a runtime that reports none (most subscription
  CLIs, local models) is counted in turns only.
- **Presence only for opening.** Under rules v2, opening an agent to
  everyone needs the owner's passkey; narrowing it and changing budgets
  don't (someone holding your device key can already post as you and use
  the agent directly). Without rules v2 no change needs presence.
- **The owner's id must be stable.** Access compares member ids (mapped
  through device links). An account id is the same on all of a person's
  devices, so this bites only after an identity reset: alone in a workspace
  (as dispatch's solo rule assumes) the device still runs an agent whose
  owner id is stale; in a room with other people the agent must be
  reassigned. The device-local workspace is never refused.
- **Duplicate refusal notes.** Two of the owner's devices that both see a
  refused turn at once may each post the note.
- **The host's person isn't asked.** An Admin can bind an agent to a host
  (S2, with presence); the host then runs it on its credentials under the
  agent owner's access setting, not the host person's.
- **Rate-limited refusals stay open.** A turn refused silently isn't
  answered; the sender's own device refuses it again (once) after the
  offline-fallback wait.

### Someone with your phone {#lost-phone}

The phone app holds a credential that lets it act through your box with the
box's role, and a Secure Enclave key certified as your device: with an
unlocked phone and the app open, someone can post, vote, answer and ask for
an apply as you; clearing a lockdown and gated votes also need your passkey.
Optional Face ID / passcode lock protects the app on an unlocked phone. Revoke the phone from the box; see
[Lost phone](runbooks.md#lost-phone).

### Someone who sees the add-a-phone QR code {#add-phone-qr}

The desktop's **Add a phone** QR code (0.1.1) carries a one-time secret for
5 minutes. Whoever reads it, and can reach a box of the workspace (a
pairing link, the box's token, or a box they run), can answer it with their
own phone. The desktop locks the code to the first phone that answers,
disables **Add this phone** for 5 seconds, aborts if a second phone answers
before approval ("Two phones answered this code"), and after approval keeps
listening until the code expires, raising a revoke alert if another phone
answers (the tripwire: "Another phone answered this code", with
**Revoke *label*** and **It's mine**). The box and
the relay can't forge, swap or replay a request without the secret, and the
box can't certify a phone. What's left is in
[Known limitations](#known-limitations) (item 32) and, in plain words, in
[Adding a phone by QR code](../../ligus/concepts/security.md#add-phone-qr).
If the alarm fires, see
[Another phone answered this code](../../ligus/features/phone-app.md#another-phone).

### Someone with a copy of a data dir {#data-dir}

A data dir (desktop or CLI) contains private key seeds, workspace keys,
tokens and every event in clear. Whoever has it **is** that device. Ligus
writes key seeds `0600` in a `0700` key dir, and `ligus enroll` writes
`config.toml` `0600` in a `0700` data dir, so other local users can't read
them; anything running as your OS user still can, agents included. Disk
encryption and OS account separation are the protection; see
[Compromised device](runbooks.md#compromised-device).

## Known limitations {#known-limitations}

Stated plainly, as of 0.2.0. Each stays on this list until a release fixes
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
   direct network, and since Phase C your whole
   [home dir](../../ligus/concepts/security.md#home-default-deny) but the
   worktree, the turn's own dirs, a read-only toolchain allow-list and
   signed grants. The sandbox shares your user and kernel: it is not a
   boundary against kernel bugs, and a same-user process outside it (an LSP
   server, your terminal) is not confined. A host without it (Windows, Linux without
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
   That is as strong as that shared workspace's binding of the account key:
   the key the account's own first word there fixes (the device that signed
   the founder's seed, or that signed the account's own admission), and a
   key someone else registers under the account is refused there without
   disputing it ([account keys](#account-keys)). When two workspaces disagree about the
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
8. **A box is never you; the phone acts as you only with its own key.** A
   `ligus serve` box is its own service member: what it sends on its own
   (its plain endpoints, its votes) is the box's, and never satisfies a
   human gate. What only a person may do the phone signs with its own
   device key ([phone actions](../reference/serve-api.md#phone-actions) and
   box admissions). Residuals: the box chooses what the phone shows, so a
   vote binds to the content digest the phone saw; a client should
   recompute `diffDigest` from the content it displays when it can (a
   workflow or command proposal's digest covers JSON a client may not
   reproduce). A box can withhold or delay a phone action. Rebinding an
   agent, inviting a person and DMs as you stay on the desktop (the phone
   tier doesn't include them; the phone isn't certified in a DM's
   workspace). An offline host named by an open apply request blocks every
   other apply of that proposal until the requester or an Admin withdraws
   it; a withdrawal racing a host that already committed is refused by the
   fold when it sorts first, and that host then takes its commit back.
   A bearer box admission (a Kubernetes Secret) can be redeemed by
   whoever holds it before the box does. Until an Admin's device has synced
   and closed the admission, a holder of the code can still date a
   redemption before the expiry, or slot one in before the box's: no clock
   on the log is trusted, so the bound is an Admin's device seeing the
   admission settle. Keep it short-lived, delete the Secret once the box has
   joined, keep an Admin's desktop syncing, and remove any service member
   you didn't expect.
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
    `LANGSTROTH_RELAY_MAX_STORE_BYTES` so writes can't exhaust its memory.
17. **The relay sees metadata** (above), including which contact ids get
    drops and when. The relay's admin
    API isn't in the open-source build, so per-person relay tokens need signed
    tokens (short-lived ones from [langstroth-auth](../deploy/ligus-auth.md), for
    example) or a restart to change an allowlist.
18. **The direct phone path is plain HTTP.** Use the relay tunnel or a
    tailnet, or a LAN you trust. Invite codes, code-host tokens and device
    sign-in codes are refused on it (tunnel, or the operator token on
    loopback, only), but chat content and the bearer token still cross it.
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
    under one id. A device that holds both denies every copy of that
    signer under the id, whatever order they came in, and coverage never
    passes through them; but a device only learns of it when the second
    copy reaches it, so devices can disagree until both copies have reached
    them. Anyone else can also publish an envelope under a real event's id,
    and that is not an equivocation: an unsigned copy is refused at ingest,
    a copy no certified key verifies never counts against a verified one,
    and a copy signed by someone else's own key is their own event, folded
    beside the real one and judged as theirs. What remains:
    - **Before the certificate arrives.** Until the certificate of the
      real event's signer has reached a device, the real copy is no more
      verified there than a forged one signed by an unknown device, and
      both are left out (or, if the forger's device is certified and the
      real one's isn't yet, the forger's copy folds as the forger's own
      event). The real event folds once the certificate arrives.
    - **A closed admission's named redemption.** A closure names the
      redemption it saw by event id. If someone holding the admission code
      copies that id with a redemption of their own, the closure names
      neither: both are refused and the Admin issues a new admission. A
      copy without a proof that verifies (the invite and redemption ids
      alone, which every keyholder sees) confuses nothing.
    - **Copies under a covered id.** A removal's watermark names a device's
      chain by event id. An id under which two copies are held for that
      device (the device equivocated, or a copy no key of it signed) is
      never covered and coverage stops there, as at a gap, so the member's
      events behind it are judged as of the cut. A second key for a device
      id can't exist: a device id is derived from its key.
    - **Readers outside the fold.** Device-local lookups that read raw
      events rather than the fold (who signed a proposal, a flow answer's
      author, a cut's watermark, the membership audit, what a peer link
      forwards) read every copy held; the audit lists only what the config
      fold allowed. A re-encoded copy of a genuine envelope (other
      whitespace or key order, same signature) is that envelope, never its
      signer's second one.
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
28. **What the home allow-list and grants leave open.** The home is
    denied by default, but some of it stays readable to every agent
    process: the [toolchain allow-list](../../ligus/concepts/security.md#home-allow-list)
    (binaries and libraries can be read and copied, never written), and
    every `PATH` entry under your home, which Ligus trusts as an install
    dir; a program's own install dir under your home is readable to that
    process. A [signed grant](../../ligus/concepts/security.md#sandbox-grants)
    applies on every host that runs the agent, including other members'
    hosts and boxes, wherever `~/…` points there; it never opens a data dir
    or credential dir. On macOS, Copilot and Antigravity keep the login
    keychain files readable because their sign-ins live there. `/usr/bin/git`
    and other `xcrun` shims print `couldn't create cache file` warnings
    under the sandbox (their cache lives in the per-user temp dir, which
    stays unwritable); the commands still run. The Landlock tier can't hide
    other processes and can't create entries at the worktree's top level;
    the bubblewrap tier is not exercised on hosts whose AppArmor restricts
    user namespaces (Ubuntu 24.04 without the profile). `git commit` inside
    the worktree stays denied by design: agents' changes are captured as a
    diff. A grant is judged where it resolves when the turn is planned and
    checked again when the Linux executors bind it (the Landlock tier also
    verifies each opened descriptor); on the bubblewrap tier a path swapped
    for a symlink in the moments between that check and bubblewrap's own
    mount, by an agent of a concurrent turn with write access beside it,
    is still bound. Other turns' scratch dirs are denied as a whole; a
    turn's own home holds its CLI's sign-in by design, so the agent in that
    turn can read it. A read-write grant of a project dir is code you will
    run: the code-path list covers your home's startup files, plugins and
    caches, not a repository's own `.git/hooks`, `.git/config`, `.envrc` or
    build files, which run when you next work in it.
29. **The shell is the Owner's, not a boundary.** For a turn another member
    starts, the [owner-only shell](../../ligus/concepts/security.md#owner-only-shell)
    takes away each CLI's shell tool, ACP `execute` requests and
    `run_command`, and refuses Codex, aider and Hermes, which can't run
    without one. Edits stay, and so do the CLIs' other tools: a tool that
    runs a program for the agent without being its shell (a build or test
    tool an MCP server offers, an editor action) is not covered, and the OS
    sandbox, not the shell gate, bounds what any of them reaches. An
    Owner's per-agent grant turns the shell on for everyone's turns of that
    agent. Build and test tools are code execution: an agent without a
    shell can still write a `build.rs`, a `conftest.py`, a `package.json`
    script or a test, and whatever later runs them (an Owner's turn with a
    shell, an approved `cargo test` command, the device-local pre-flight
    test, your own terminal) runs that code, inside the sandbox when Ligus
    starts it and with your full access when you do. The gate stops a
    non-Owner's agent from running commands now; it does not make the code
    it writes safe to run. Review the diff before you build it. An MCP
    server that offers a shell is told apart by its tool names (`bash`,
    `exec`, `run_command`, `terminal`, ...) and offered only to turns with
    the shell; a server that names its shell something else still gives one
    to every member's turns, so allow-list only servers whose tools you
    know. The workspace's signed grants reach a member's host only when that
    device applies them: by default the Owner's devices do and every other
    member's don't, a device's user chooses in Settings → Sandbox (a
    headless host with `LIGUS_SANDBOX_GRANTS`), and a change shows a notice
    until it is reviewed.
30. <a id="lamport-bounds"></a>**Lamport bounds stop a forward-dated clock, not a backdated one.**
    Every event names the newest event of its log its author held
    (`logHead`) and its lamport is exactly one past it, so a lamport is
    never more than the longest chain of real events on its log (a lamport
    near `u64::MAX` is refused and moves no one's clock), and an event can
    sort after events written after it only by counting from a chain of
    real events that long. What stays an author's choice:
    - **Chains of their own events.** The bound counts every real event,
      whoever wrote it and whatever its verdict. An author, or a box with
      its own key drafting a phone's action, can write `k` events of its
      own, each one past the last, and have the withheld action name the
      last: released later, it sorts after up to `k` events written after
      it was drafted (a stale approve can beat a later reject from another
      device). It costs `k` signed events that every device stores and
      that show in the log (`k` events released at once is the sign), not
      a single forged number. The phone refuses a lamport below the last
      one it signed on the log, so a box can't sort a phone's new action
      before the phone's earlier one.
    - **Backdating within what they held.** An author can name an older
      head (an offline device does this honestly), so a race that "the
      first in canonical order wins" goes to whoever drafts earliest or
      backdates: concurrent lease successions, turn claims, admission
      redemptions (an Admin's close decides those), and two concurrent votes
      of one person (there a rejection wins whatever the ids).
    - **Concurrent writes that apply to everything before them.** Switching
      dearth mode off lands every pending member that sorts before the
      switch, including one added concurrently that its author never saw.
      Whole-list updates (skills, vault sources, workflow definitions, the
      channel order) are last-writer-wins and can drop a concurrent edit.
      A lockdown clear no longer has this problem: it names its raises.
    - **Holds.** An event whose log head hasn't reached a device is held
      there until it does. An event ingest can't judge yet (a device not
      certified for the account it claims, an account not admitted yet, a
      disputed account key) waits in the device's held table, never before
      the sync cursor, and is judged again when the roster changes and every
      10 minutes ([held events](#held-events)). Whether a device refuses an event at ingest
      depends on its roster when the event arrives (a revocation, a removed
      member, a rebound agent key, a certificate the relay delivered late),
      so a device keeps every event it refuses, for any reason, as an inert
      witness (its id, lamport, log head and chain link, kept by that whole
      structure, so a forged copy of an id never shadows the genuine one),
      and every chunk it prunes too: whatever another device accepted and
      counted from, this one counts from as well. A device never counts its
      own next event from a witness (it doesn't push the events it
      refuses). What can still hold: a relay or peer that withholds an
      event from one device holds there the events that count from it,
      until it arrives; once every device has every body, every device
      judges the log alike. A forged head the author never publishes holds
      that author's event for good, within 4096 of the newest event; past
      that it is refused.
    - **Ties.** Events at one lamport sort by event id, which their drafter
      picks (a box drafting a phone's action included).

31. **Agent budgets are per device and approximate.** Agent access and the
    daily budgets are enforced on the device that runs the agent, counted in
    its own database (UTC days), and estimated from reported token usage; see
    [A member spending someone else's credentials](#agent-use).

32. **Adding a phone by QR rests on the fingerprint comparison.** Someone
    who reads the desktop's QR code in its 5 minutes (a photo, a screen
    share) and reaches a box of the workspace can answer it. The hold and
    the tripwire catch it when your own phone also answers; they can't when
    the person approves without comparing fingerprints and never scans with
    their own phone, or when the attacker runs the only box the real phone
    can reach and holds its request back. The extra phone is listed in My
    devices, and the real phone keeps being refused at pairing. Content
    protection on the QR is best effort. The relay or any member can
    overwrite the presence slots the request travels in (availability only:
    forged content fails the checks). A co-member who gets a person to scan
    their QR, past the warnings, binds that phone to their own account and
    locks its person out of the workspace until they reset the phone's
    device key. The relay sees that a phone is being added, by which box and
    when.
33. **The paste fallback can lock a phone out.** A phone's `ligusphone1:`
    code isn't secret (the phone shows it). Anyone in the workspace who has
    it can certify it for their own account; the phone then has two holders,
    its person can't act from it in that workspace, and they reset the
    phone's device key. The QR flow isn't affected. A fix is planned after
    0.1.1.
34. **Older members can still show a forged proposal comment.** Since 0.2.0
    a proposal comment whose named author isn't its signer is refused, and
    only verified comments are forwarded to an agent. The check runs on each
    member's own device, so a 0.1.x desktop or `ligus serve` box in the
    workspace can still show a comment under someone else's name and forward
    it to an agent when its user asks for a revision. Upgrade every member.
35. **Another workspace's member can disturb a chat on one device.** A
    member of another workspace you share who knows a chat's id can disturb
    that chat on one device. Fixed in 0.2.1.

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
| Relay | A malformed `LANGSTROTH_RELAY_TOKEN_PUBKEY` fell back to open | The relay refuses to start |
| Relay | Client-supplied `Fly-Client-IP` was trusted everywhere | Only on Fly; elsewhere `LANGSTROTH_RELAY_TRUSTED_PROXIES` decides |
| Relay | Unbounded ids, list responses and memory-store growth | Id limits (`400`), `LANGSTROTH_RELAY_MAX_LIST_BYTES`, `LANGSTROTH_RELAY_MAX_STORE_BYTES` (`507`) |
| Relay | The Helm chart's tunnel defaulted to open | Off, like the binary |
| Core | Proposal-set vote roles were self-declared, so a Viewer's vote claiming Admin met an Admin floor | A vote counts only from the voter it names, at their role on the roster when they cast it, for the content digest it names; status is derived from counted votes |
| Core | Hoisted checks were retroactive: a later demotion undid decisions, a later promotion validated earlier steps | Every event is judged at its own position on the workspace config log; events whose position hasn't synced wait instead of being dropped |
| Core | Key possession alone let a non-member or ex-member post and self-enroll | Every author must be a member at the event's position; joining needs an Admin's single-use invite |
| Core | Signatures covered re-encoded JSON, so fields an older build didn't know were lost or broke verification | Envelopes are verified, stored and re-sent as the bytes received |
| Core | A removal in one workspace revoked devices in another; the identity mode came from the environment | Trust rosters are per workspace with the founder pinned from the invite; folds never read the environment |
| Host | Agents could read the data dir (keys, settings, `serve.token`) and reach the network directly | Agent processes run in the OS sandbox with a per-turn egress proxy and an allow-listed environment |
| Host | The tool role gate only held on headless hosts and read today's roster | One author gate on every host, at the author's role when the message was posted |
| Host | Repository-supplied runtimes and MCP servers ran on clone | Inert until enabled on this device by digest |
| Host | Votes through an owner-enrolled serve box counted as the owner's | A vote counts only when a certified device of its voter signs it (the phone, with its own key); the box's vote, lockdown-clear and answer routes (and `presence/prepare` for them) answer 410 |
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
| Sandbox (Phase C, home) | Agent processes could read everything in your home outside a short deny list: documents, other repositories and other workspaces' worktrees, `~/.gitconfig` (URL rewrites with tokens), `~/.npmrc`, `~/.cargo/credentials.toml`, tool configs under `~/.config` | The home is default-deny for every agent process on Seatbelt, bubblewrap and Landlock; each child gets a per-turn home; a read-only allow-list of toolchain install dirs, each with its reason, and signed grants let paths back in; a grant never opens a data or credential dir. Verified by `sandbox_home_is_default_deny`, `sandbox_home_grants_open_only_what_they_name`, `sandbox_turn_home_has_only_the_git_identity`, `bwrap_home_is_a_tmpfs_and_hard_denies_come_last`, and the `sandbox_home_regression` runs (git, `cargo build`, node, pip into a venv, Claude Code `-p`, `codex login status`) |
| Sandbox (Phase C review, allow-list) | `cargo` (and every program under `~/.cargo/bin`) opened `~/.cargo`, `credentials.toml` included, to an approved command or MCP server it ran as; a real launcher in `~/.local/bin` opened all of `~/.local`. An allow-list entry or grant that was a symlink to the home (a symlinked `~/.local/bin`) opened the whole home on every tier, and on the Landlock tier one through a link into `~/.ssh` opened it. A read-write grant of the home's parent (`/home`) brought the home back read-write on the bubblewrap tier. A read-write grant of a `PATH` dir, a shell startup file or a login item let an agent plant code that later runs as you, unconfined, which put an Admin's grant above the Owner-only shell | A program's prefix never climbs to a dir directly in the home; every path that opens part of the home is judged where it resolves (never the home, a dir above it or a denied path); read-write grants stay read-only on `PATH`, toolchain, shell startup, autostart and tool-config paths. Verified by `review_program_prefix_never_opens_a_tool_home(_live)`, `review_symlinked_allow_entries_resolve_before_they_open(_live)`, `review_a_grant_above_the_home_opens_nothing_in_it(_live)`, `review_rw_grants_never_open_code_that_runs_outside_the_sandbox(_live)` and `review_grant_case_variants_open_no_denied_path` |
| Host (Phase C review, shell) | A CLI Ligus starts outside any turn (the chat summarizer and compaction, chat titles, a runtime's Test) ran with the baseline policy, whose shell was on: Claude Code got Bash for a prompt built from other members' messages. `ligus doctor --sandbox` and Settings → Sandbox showed the login keychain readable for a CLI signed in by file | No turn in scope means no shell (`turn_shell_allowed`); the doctor's turn test runs in a turn of its own; the effective policy predicts the file sign-in without reading the keychain. Verified by `no_turn_in_scope_means_no_shell` and `review_effective_policy_shows_the_keychain_denied_for_file_sign_in` |
| Sandbox (Phase C second review, protected links) | A protected path in the worktree (`.envrc`, `ligus.config.toml`, `.claude`, ...) that was a symlink out of it, committed to a repository, was bound read-only at its resolved target after the home's tmpfs on the bubblewrap tier: `.envrc -> ../../.ssh` exposed `~/.ssh` to every turn on that checkout (pre-existing). On every tier the link itself could be replaced | A turn that may write a checkout whose protected path is a symbolic link is refused, naming the path. Verified by `review2_protected_symlink_out_of_the_worktree_opens_nothing_live` (Seatbelt, Landlock, bubblewrap) |
| Sandbox (Phase C second review, scratch dirs) | Every turn's scratch dir sat side by side in the system temp dir, readable by any other turn's processes: a concurrent turn's home held its CLI's sign-in (Copilot's `hosts.json`, Codex's `auth.json`) | Turn dirs live in one private `ligus-turns-<uid>` dir that every executor denies but the turn's own (bubblewrap: an empty tmpfs; Landlock: inode rules, so later dirs are outside them). Verified by `review2_a_turn_never_reads_another_turns_scratch_live` |
| Sandbox (Phase C second review, grants) | A read-write grant spelled in another case (`~/Library/launchagents`, `~/.SSH`) for a dir that didn't exist yet passed the code-path and deny checks on macOS and let an agent create a login item; a grant of `~/.cargo`, `~/.m2` or `~/.npmrc` read the registry tokens in it; read-write grants of a Python user site, `~/.cargo/registry`, editor tasks and extensions, `~/bin` or Homebrew's prefix let an agent plant code that later runs unconfined; a grant swapped for a symlink between the plan and the Linux executors' binds opened its target; with `HOME` unset or wrong the real home wasn't denied | Paths compare without case on macOS and Windows; credential files beside tool config are hard denies; the code-path list covers interpreter startup and site dirs, package caches, editors, terminals, tool plugins and writable install prefixes; the plan keeps resolved paths and the Linux executors skip any that moved (Landlock verifies each opened descriptor); the password database's home is denied too. Verified by `review2_rw_grant_case_variants_never_reach_code_or_denied_paths_live`, `review2_a_grant_of_a_tool_dir_never_opens_its_tokens_live`, `review2_rw_grants_never_open_more_code_paths`, `review2_a_path_swapped_after_the_plan_opens_nothing` and `review2_a_wrong_home_still_denies_the_real_one_live` |
| Host (Phase C second review, shell) | A CLI started outside a turn on the legacy (non-ACP) path kept the shell its runtime args named (Copilot's `--allow-tool=shell`, Antigravity's `--mode auto`, pi's bash), and the summarizer and titler ran with the checkout as their working dir (a title, synced to every member, could carry a file from it). An MCP server whose tool was a shell gave one to every member's turns. A signed grant applied on every member's host with no notice or opt-out | Outside a turn the shell is switched off by flag where the CLI has one; the summarizer and titler run in their scratch dir. MCP tools named like a shell are offered only to turns with the shell, and a CLI turn without it gets only servers seen to list none. Each device decides whether it applies the grants (on by default only for the Owner's devices), shown in Settings → Sandbox and `ligus doctor`, with a notice when they change. Verified by `review2_outside_a_turn_a_cli_has_no_shell`, `shell_tools_are_flagged_by_name`, `grants_apply_by_default_only_on_the_owners_devices` and the Settings tests |
| Tests (credentials) | Nearly every executor test spawned a CLI turn against the real home, which bridged Claude Code's login-keychain item and copied the real CLI sign-ins into temp turn homes | A test run (this crate's unit tests, nextest, any binary cargo built for a test, in the `deps` layout or the newer `build/<pkg>/<hash>/out/` one, so every crate's and every integration test) copies no real sign-in, reads no keychain item and opens no OS secret store (owner-only files instead) unless `LIGUS_TEST_KEYCHAIN=1`. Verified by `review2_tests_never_carry_real_sign_ins` and `tests/test_hygiene.rs` |
| Host (doctor) | `ligus doctor --turn-test` reported leaks on the bubblewrap tier that weren't: pid 1 in the turn's own pid namespace is bubblewrap, and `ls` of the data dir lists the empty tmpfs over it | The probe counts a readable environ only in the host's pid namespace and a data dir only when it shows an entry. Verified by `probe_script_reports_only_real_leaks` |
| Host (Phase C, shell) | Any member at the write floor (Admin) drove a CLI's shell tool or `run_command` on the host's machine | The shell is on only when every person behind the turn is the workspace Owner (the device owner in the local workspace) or an Owner granted it to the agent with a passkey; otherwise each CLI runs without its shell, ACP `execute` is denied, and Codex, aider and Hermes are refused. Verified by `the_shell_is_the_owners_unless_granted`, `shell_free_args_cap_every_cli_at_edits`, `no_shell_takes_bash_away`, `shell_role_is_the_workspace_role_outside_the_local_workspace` and the `sandbox_grants_fold` tests, including a property test against a reference model |
| Sandbox (unconfined hosts) | Without an OS sandbox only write-capable CLI turns were refused: read-only CLI turns ran with nothing but an env allow-list and could read `ligus.db` (not encrypted at rest), keys and tokens; `run_command`, approved commands, pre-flight tests and stdio MCP servers ran unconfined at any tier | Every agent process is refused at any tier, with a note naming the platform and the fix; HTTP models get no built-in file tools and run as plain chat. Verified by `unconfined_refuses_read_only_cli_turns`, `unconfined_refuses_tool_purpose` and `unconfined_allows_toolless_api_chat` |
| Core (identity binding) | Ingest checked a stamp's account against the signing device only when the stamp carried one, so a member's device could stamp another member's actor id (their account id) with no account: the fold judged a `MemberRoleChanged` as that member (a Contributor could promote themselves to Owner) and the author gate gave the turn their role | A stamp whose actor id is a known account the signer doesn't sign for is held and never applied (since the review of forged copies: held rather than quarantined, so the verdict never depends on which certificates arrived first). The author gate counts a member with an account only when the stamp names that account and the signer is certified for it and unrevoked at the stamped position. Verified by `a_stamp_naming_another_member_without_their_account_gets_no_role` and `chain_loops_and_overlong_chains_fail_closed` |
| Host (board pickup) | Automatic board pickup posted the item's task (a proposal's or failed stage's title, written by whoever filed it) as the host's own user, so the author gate's own-user rule ran it at Owner: anyone could title a proposal with instructions an Owner's desktop then ran at full tier | An automatic pickup is posted as a runtime effect with no role, below every floor; a manual pickup is unchanged. Verified by `auto_pickup_is_not_the_host_user_speaking` |
| Core (event clocks) | An author chose an event's lamport, so an event drafted early and withheld (by its author, or by a box holding a phone-signed action) could be given a huge lamport and sort after genuinely newer events: a stale approve beat a later reject, a delayed `ChatLockdownCleared` lifted a newer lockdown (it cleared every lockdown on the chat), and a lamport near `u64::MAX` dragged every honest clock after it | A stamp names its log head and the lamport is one past it, checked by every fold and by the phone before it signs; an event out of bounds is refused and is no position. A clear names the raises it lifts. Between one person's concurrent votes a rejection wins. Verified by `prop_no_honest_history_is_denied`, `prop_an_inflated_lamport_changes_nothing_else`, `a_withheld_vote_sorts_where_it_was_drafted`, `a_withheld_clear_lifts_only_what_its_author_saw` and `between_concurrent_votes_of_one_person_a_rejection_wins` |
| Core (chain workspace) | The trigger chain judged each event on the config log of the workspace that event named, so an event in a chat stamped with another workspace its author owned was judged as Owner there; the chain's config cache answered every workspace with the first one it loaded | Events stamped with a workspace other than the chat's fail the chain closed, a workflow run's first record must name the chat's workspace, and the cache is keyed by workspace. Hardening: such an event doesn't fold into the chat today. Verified by `an_event_stamped_with_another_workspace_fails_closed` |

| Core (forged copies) | Anyone holding the workspace key could erase any event: an unsigned envelope (or one no key verifies, or one signed by a device of their own) under a real event's id was accepted, recorded as an equivocation, and both copies were then denied on every device, so a `MemberRemoved`, a reject vote, a lockdown raise or the config seed vanished | Only the same signer's two verified copies are an equivocation. Unsigned envelopes are refused at ingest; a copy no certified key verifies never displaces a verified one; another signer's copy under the id is their own event, and every fold structure (denials, coverage, cut identity, verdicts, the frontier's lamport) is keyed by copy. Every reader that must not be blinded by a copy that arrived first reads all copies. Verified by `a_forged_copy_never_erases_an_honest_title_change`, `a_forged_copy_never_erases_a_removal`, `a_forged_copy_of_the_seed_leaves_the_workspace_standing`, `prop_forged_copies_never_erase_honest_events`, `the_reviewers_probe_an_honest_title_change_survives_a_forged_copy_in_either_order` and `prop_devices_fold_the_honest_log_whatever_arrives_first` |

| Core (device-id squatting) | Device ids were chosen by the device, so a member could certify another member's device id (or one that would be theirs) under their own account with a key of their own. The fold then checked the victim's key events against the squatter's key, and the trust roster took whichever certificate sorted first (a backdated copy did): every event of the victim's device was refused everywhere, and the squatter could sign as the victim, or certify a key for an agent hosted on the victim's device, through its id. Revoking a device the squatter's account had certified was self-service, so any Contributor could revoke anyone's device, the founder's included | Refused by construction: a device id is derived from the device's first signing key (UUIDv8 of `SHA-256("ligus-device-id-v1\0" ‖ alg ‖ "\0" ‖ key)`, every device kind, phones from their Secure Enclave key), and a certificate whose id isn't its key's never verifies, so it is refused in the fold, at ingest and in the roster wherever it sorts. One device id names one key, and only that device signs under it. Certifying someone else's real device key for your own account signs nothing, and who holds a device is read from the whole log (the accounts the device itself vouched for), never from a position: such an account can't revoke the device as theirs, run an unbound agent on it, or make the device's own account's events wait or fail for good. A device several members certified that vouched for none of them (a phone, a box its person's desktop enrolled) is no one's own to revoke: it goes down the ranks over all of them (final review: an adopter could revoke anyone's phone, `an_adopter_cannot_revoke_a_phone_that_vouched_for_no_one`). Verified by `a_backdated_squat_is_refused_wherever_it_sorts`, `a_squatter_cannot_sign_as_the_victim_through_the_victims_device_id`, `a_squatter_cannot_certify_a_key_for_an_agent_on_the_victims_device`, `a_squatter_cannot_revoke_the_victims_device_as_their_own`, `certifying_someone_elses_device_key_holds_nothing_backdated_or_not`, `a_squatters_events_under_the_victims_device_id_do_not_uncover_the_victim`, `a_backdated_squat_neither_erases_nor_impersonates_at_ingest`, `a_victims_event_before_its_certificate_is_held_not_refused_after_an_adoption` and `device_id_vectors_derive_as_frozen` |
| Core (held events, dispute DoS) | An event ingest held (a certificate or admission not here yet) pinned the sync cursor until it could be judged, so one whose certificate or admission never comes (a forged room founder, a member signing as an account that never certifies them, a removed member never re-admitted) stopped a device from syncing anything after it, for good. And a member could register a second key under anyone's account id from a fresh device (or forge a rotation): the account became disputed and every event its owner signed was refused, a one-event way to silence any member | Held events wait in a bounded device-local table (1,000 per signer, 10,000 and 32 MiB per workspace, 50,000 and 128 MiB per device, 30 days; first come stays, drops counted in `ligus doctor` and Settings > Team sync) and the cursor moves past them; they are judged again by the same gate when the roster changes and on a timer, so a released event folds as if it arrived in order. Account keys follow one rule set everywhere (`ligus_core::account_keys`): a registration counts only when its signer published its own certificate under that key; the account's first word (the founder's seed, its own admission) anchors its key; any other key is refused and attributed to its signer, wherever it sorts, and never disputes the account. Only the account's own lineage asserting two keys (its own device registering another key, a key designating two successors) is a dispute, and a dispute holds rather than refuses. An uncertified device's event claiming any account is held (it was accepted while the account wasn't known yet). Verified by `prop_devices_converge_whatever_arrives_first`, `a_member_cannot_silence_another_by_registering_a_key_for_them`, `events_whose_certificate_never_comes_do_not_stall_sync`, `a_flood_of_held_events_is_bounded`, `a_registration_of_someone_elses_account_is_refused_wherever_it_sorts`, `prop_forgeries_never_dispute_or_take_a_self_enrolled_account` and `prop_a_late_anchor_ends_where_an_early_one_does` |
| Core (held events and account keys, review) | The review of the fix above found six more. A founding seed signed by any device that certified itself for the founder's account joined the trust roster, and one with a smaller event id became its base: without a founder pin the founder's account was anchored to that member's device (they signed as the Owner), with one the founder had no key and a fresh device held the whole workspace. A copy of a rotation the account already made (K0 to K1, after K1 to K2) read as K0 designating a second successor: the account was disputed at ingest and its owner's events held, and the ingest roster counted rotations under rules v2 that the config fold denies. An account whose admission arrived before its own certificate took a key someone registered in advance. A member flooding the held table from 10,000 fresh device ids made later honest holds drop, so that device never applied them while others did. A key registration whose account a forgery had disputed first was held rather than stored, the config fold skipped registrations sorting before its seed, and ingest read a signer's equivocated copies of a registration the fold leaves out, so devices (and ingest and the fold) could hold different keys for an Admin-added account. And a person invite's id is on the log, so a member who backdated a redemption of someone's invite took that admission and the account's anchor | A founding seed joins the roster only when its signer vouches under the founder pin's key, or chains under the account's key. A rotation to a key the account already held changes nothing, and under rules v2 only a rotation its config log allowed counts at ingest, as in the fold. An anchored account has no key until its anchor device's own certificate is on the log. A dropped held event that came from the relay keeps a bodyless row (its sequence) and is fetched again, at most three times, once its signer signs for the account it claims. Account key events and certificates are never held (stored inert, judged by the rules everywhere), the fold judges registrations before its seed as ingest does, and equivocated copies count for no reader. Person invites carry a redeem key (`JoinApproved.redeemKey`, additive; the redemption proof under `ligus-person-invite-redeem-v1`), whose secret rides only in the invite code. Verified by `a_competing_founding_seed_never_anchors_the_founder_to_someone_else`, `a_replayed_rotation_never_disputes_the_account`, `a_replayed_rotation_is_the_same_step_not_a_fork`, `an_anchor_whose_key_has_not_arrived_pins_nothing_else`, `a_flood_never_loses_an_honest_held_event`, `a_member_cannot_steal_a_keyed_person_invite` and `prop_the_readers_of_account_keys_agree` |

### Open {#findings-open}

These are in [Known limitations](#known-limitations) and stay there until a
release fixes them: invite codes carrying every epoch key, directory keys
for people you share no workspace with (unverified, sealed only after a
warning), a CLI-founded workspace staying unsecured until its founder adds a passkey,
devices without a founder pin keeping the passphrase key, and ingest-time
decisions that depend on arrival order, the reach of a removal's watermark (its
author's word, and chats the remover never synced), equivocations seen only
where both copies meet, a member certifying someone else's phone key for
their own account (a phone never vouches for its account on the log, so
both accounts hold it: neither can revoke it as their own, so it is revoked
by someone who outranks both, or by its person once that member is
removed, and its pairing names no one until then), an Admin revoking a device that isn't
certified on the workspace yet (a device id is derived from its key, so
one who knows the key can name the device before it joins), a member who
backdates a redemption of a person invite issued before redeem keys taking
that admission and with it the account's key anchor there, accounts an Admin
added directly (only in older logs) having no anchor (a forged
self-certified registration still disputes them, fail closed), held events
given up past every cap, three refetches or the 30-day limit, what lamport bounds leave to an author (backdating,
concurrent writes that apply to everything before them, holds), human
clicks that run others' text at the clicker's tier, and unconfined LSP
servers and terminal. On Kubernetes, DNS tunnelling through
the cluster resolver remains possible (see
[Kubernetes](../deploy/kubernetes.md#residual-risks)).
